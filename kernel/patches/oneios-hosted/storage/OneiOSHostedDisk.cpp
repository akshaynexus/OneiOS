// SPDX-License-Identifier: BSD-2-Clause-Patent
//
// OneiOSHostedDisk: every disk xnuhost serves to the hosted kernel (runtime/xnuhost/src/virtio_blk.c: virtio-blk
// on a virtio-mmio window), as one IOBlockStorageDevice class. The read-only images of the set (os.img,
// systemos.img, app.img), the Data seed (a tar on a raw disk) and the writable Data store.
//
// After UniXNU's NativeVirtioBlock (https://github.com/UniXNU/VirtioMmioBlock 4e1013e, BSD-2-Clause-Patent):
// the register sequence and the polled split queue are its own. What differs:
//   - The registers are reached through xnuhost's mmio_read/mmio_write host calls (oneios_hosted_mmio_*,
//     osfmk/arm64/hosted/hosted_blk.c), not through a mapping: a mapped device page of a hosted kernel is a
//     fault per access. xnuhost serves a queue inside the notify call, so a request is done when it returns.
//   - Writes and flush, for a disk the device does not offer as read-only.
//   - Verified root: xnuhost checks every chunk of a read-only image against the image set's hash lists before
//     the guest sees it (runtime/xnuhost/src/imageset.c) and says so in the disk's device tree node
//     ("oneios,verified" = the SHA-256 of the hash list). That property is this disk's policy: no signed block
//     hashes are checked a second time here. xnuhost holds the kernel's memory, so it is the root of trust.
//   - Which disk is which comes from the node too: "oneios,role" (os, systemos, app, data, seed) and
//     "oneios,volume-uuid" (the APFS volume to mount from an image).
#include <IOKit/IOBufferMemoryDescriptor.h>
#include <IOKit/IODeviceMemory.h>
#include <IOKit/IOLib.h>
#include <IOKit/IOBSD.h>
#include <IOKit/storage/IOBlockStorageDevice.h>
#include <libkern/c++/OSData.h>
#include <libkern/c++/OSDictionary.h>
#include <libkern/c++/OSIterator.h>
#include <libkern/c++/OSNumber.h>
#include <libkern/c++/OSString.h>
#include <libkern/crypto/sha2.h>
#include <kern/clock.h>
#include <kern/task.h>
#include <sys/types.h>
#include <sys/param.h>

extern "C" {
uint32_t oneios_hosted_mmio_read32(uint64_t physical);
void oneios_hosted_mmio_write32(uint64_t physical, uint32_t value);

int native_verified_block_root_authorized(dev_t device);
int native_verified_block_manifest_sha384(dev_t device, const char *uuid, uint8_t digest[48]);
int native_verified_block_volume_device(const char *uuid, dev_t *device, char *path, size_t size);
int oneios_hosted_volume_uuid(const char *role, char uuid[37]);
int oneios_hosted_disk_device(const char *role, dev_t *device, char *name, size_t size, uint64_t *bytes, char sha256[65]);
int oneios_hosted_data_volume(dev_t *device, char *path, size_t size);
int oneios_hosted_disk_zero(const char *role, uint64_t bytes);

volatile uint32_t oneios_hosted_disks = 0;
volatile uint32_t oneios_hosted_disk_failures = 0;
volatile uint64_t oneios_hosted_disk_requests = 0;
}

// VirtIO 1.2 MMIO, modern split queue. All numbers here are protocol offsets.
namespace {
constexpr unsigned queueSize = 8;
constexpr unsigned transferSize = 1U << 20;
constexpr unsigned maxDisks = 16;
enum : uint32_t { opRead = 0, opWrite = 1, opFlush = 4 };
enum : uint32_t { featureReadOnly = 1U << 5, featureFlush = 1U << 9 };
struct Descriptor { uint64_t address; uint32_t length; uint16_t flags; uint16_t next; };
struct Request { uint32_t operation; uint32_t reserved; uint64_t sector; };
struct Used { uint32_t id; uint32_t length; };
static_assert(sizeof(Descriptor) == 16 && sizeof(Request) == 16 && sizeof(Used) == 8, "VirtIO wire layout");
inline void dmaBarrier() { __asm__ volatile("dmb osh" ::: "memory"); }
const char apfsVolumeContent[] = "41504653-0000-11AA-AA11-00306543ECAC";
}

class OneiOSHostedDisk : public IOBlockStorageDevice {
  OSDeclareDefaultStructors(OneiOSHostedDisk)
  IOBufferMemoryDescriptor *queueMemory = nullptr;
  IOBufferMemoryDescriptor *transferMemory = nullptr;
  IOLock *gate = nullptr;
  uint64_t registers = 0;            // the window's physical address
  uint64_t queuePhysical = 0;
  uint64_t transferPhysical = 0;
  uint64_t capacity = 0;             // sectors
  uint16_t availableIndex = 0;
  uint16_t usedIndex = 0;
  bool online = false;
  bool writable = false;
  bool flushes = false;
  bool verified = false;
  uint8_t verifiedDigest[32] = {};
  char role[16] = {};
  char volumeUUID[37] = {};
  char seedSHA256[65] = {};
  uint64_t contentBytes = 0;
  uint32_t read32(unsigned offset) const { return oneios_hosted_mmio_read32(registers + offset); }
  void write32(unsigned offset, uint32_t value) { oneios_hosted_mmio_write32(registers + offset, value); }
  void writeAddress(unsigned offset, uint64_t value) {
    write32(offset, static_cast<uint32_t>(value));
    write32(offset + 4, static_cast<uint32_t>(value >> 32));
  }
  bool reset();
  bool configureQueue();
  void identity(IOService *provider);
  IOReturn transfer(uint32_t operation, uint64_t sector, uint32_t bytes);
public:
  static OneiOSHostedDisk *disks[maxDisks];
  static OneiOSHostedDisk *withRole(const char *wanted);
  static OneiOSHostedDisk *ofMedia(IOService *media, unsigned *depth = nullptr);
  bool isVerified() const { return online && verified && !writable; }
  const uint8_t *digest() const { return verifiedDigest; }
  const char *diskRole() const { return role; }
  const char *diskVolumeUUID() const { return volumeUUID; }
  const char *diskSeedSHA256() const { return seedSHA256; }
  uint64_t diskBytes() const { return contentBytes ? contentBytes : capacity * 512; }
  IOReturn zero(uint64_t bytes);

  IOService *probe(IOService *, SInt32 *) override;
  bool start(IOService *) override;
  void stop(IOService *) override;
  void free() override;
  IOReturn doEjectMedia() override { return kIOReturnUnsupported; }
  IOReturn doFormatMedia(UInt64) override { return kIOReturnUnsupported; }
  UInt32 doGetFormatCapacities(UInt64 *, UInt32) const override { return 0; }
  char *getVendorString() override { return const_cast<char *>("OneiOS"); }
  char *getProductString() override { return const_cast<char *>("xnuhost disk"); }
  char *getRevisionString() override { return const_cast<char *>("1.0"); }
  char *getAdditionalDeviceInfoString() override { return role; }
  IOReturn reportBlockSize(UInt64 *value) override { *value = 512; return kIOReturnSuccess; }
  IOReturn reportEjectability(bool *value) override { *value = false; return kIOReturnSuccess; }
  IOReturn reportMaxValidBlock(UInt64 *value) override { *value = capacity - 1; return kIOReturnSuccess; }
  IOReturn reportMediaState(bool *present, bool *changed) override {
    *present = online; if (changed) *changed = false; return kIOReturnSuccess;
  }
  IOReturn reportRemovability(bool *value) override { *value = false; return kIOReturnSuccess; }
  IOReturn reportWriteProtection(bool *value) override { *value = !writable; return kIOReturnSuccess; }
  IOReturn doSynchronizeCache() override;
  IOReturn doSynchronize(UInt64, UInt64, IOOptionBits) override { return doSynchronizeCache(); }
  IOReturn getWriteCacheState(bool *value) override { *value = writable && flushes; return kIOReturnSuccess; }
  IOReturn setWriteCacheState(bool) override { return kIOReturnUnsupported; }
  IOReturn doAsyncReadWrite(IOMemoryDescriptor *, UInt64, UInt64, IOStorageAttributes *, IOStorageCompletion *) override;
};
OSDefineMetaClassAndStructors(OneiOSHostedDisk, IOBlockStorageDevice)
OneiOSHostedDisk *OneiOSHostedDisk::disks[maxDisks];

static bool compatibleWith(IOService *provider, const char *name)
{
  OSData *compatible = OSDynamicCast(OSData, provider->getProperty("compatible"));
  size_t length = strlen(name) + 1;
  return compatible && compatible->getLength() == length && !memcmp(compatible->getBytesNoCopy(), name, length);
}

IOService *OneiOSHostedDisk::probe(IOService *provider, SInt32 *score)
{
  // The read-only disks and the Data store, as runtime/loader/src/machine.c names them.
  if (!compatibleWith(provider, "virtio,mmio") && !compatibleWith(provider, "droidvm,data-store")) return nullptr;
  return IOBlockStorageDevice::probe(provider, score);
}

bool OneiOSHostedDisk::reset()
{
  if (!registers) return true;
  online = false;
  write32(0x70, 0);
  return read32(0x70) == 0;
}

bool OneiOSHostedDisk::configureQueue()
{
  if (!reset()) return false;
  write32(0x70, 1);
  write32(0x70, 3);
  write32(0x14, 0);
  uint32_t lowFeatures = read32(0x10);
  write32(0x14, 1);
  uint32_t highFeatures = read32(0x10);
  if (!(highFeatures & 1)) return false;                    // VIRTIO_F_VERSION_1
  writable = !(lowFeatures & featureReadOnly);
  flushes = (lowFeatures & featureFlush) != 0;
  write32(0x24, 0);
  write32(0x20, lowFeatures & (featureReadOnly | featureFlush));
  write32(0x24, 1);
  write32(0x20, 1);
  write32(0x70, 11);
  if (!(read32(0x70) & 8)) return false;
  write32(0x30, 0);
  if (read32(0x44) || read32(0x34) < queueSize) return false;
  queueMemory = IOBufferMemoryDescriptor::inTaskWithOptions(kernel_task,
    static_cast<IOOptionBits>(kIODirectionInOut) | static_cast<IOOptionBits>(kIOMemoryPhysicallyContiguous), 4096, 4096);
  transferMemory = IOBufferMemoryDescriptor::inTaskWithOptions(kernel_task,
    static_cast<IOOptionBits>(kIODirectionInOut) | static_cast<IOOptionBits>(kIOMemoryPhysicallyContiguous), transferSize, 4096);
  if (!queueMemory || !transferMemory) return false;
  IOByteCount queueLength = 0, transferLength = 0;
  queuePhysical = queueMemory->getPhysicalSegment(0, &queueLength);
  transferPhysical = transferMemory->getPhysicalSegment(0, &transferLength);
  if (!queuePhysical || !transferPhysical || queueLength < 4096 || transferLength < transferSize) return false;
  bzero(queueMemory->getBytesNoCopy(), 4096);
  // No used-buffer interrupts: the host serves the queue inside the notify call.
  *reinterpret_cast<volatile uint16_t *>(static_cast<unsigned char *>(queueMemory->getBytesNoCopy()) + 128) = 1;
  availableIndex = usedIndex = 0;
  dmaBarrier();
  write32(0x38, queueSize);
  writeAddress(0x80, queuePhysical);
  writeAddress(0x90, queuePhysical + 128);
  writeAddress(0xa0, queuePhysical + 256);
  write32(0x44, 1);
  bool stable = false;
  for (unsigned attempt = 0; attempt < 8; ++attempt) {
    uint32_t generation = read32(0xfc);
    uint64_t low = read32(0x100), high = read32(0x104);
    if (generation == read32(0xfc)) {
      capacity = low | (high << 32);
      stable = true;
      break;
    }
  }
  if (!stable || !capacity || capacity > UINT64_MAX / 512) return false;
  write32(0x70, 15);
  online = (read32(0x70) & 15) == 15;
  return online;
}

// One request: header, the transfer buffer (not for a flush), status. Called with the gate held.
IOReturn OneiOSHostedDisk::transfer(uint32_t operation, uint64_t sector, uint32_t bytes)
{
  if (!online) return kIOReturnNotReady;
  if (operation != opFlush && (!bytes || bytes > transferSize || (bytes & 511) || sector >= capacity ||
      bytes / 512 > capacity - sector)) return kIOReturnBadArgument;
  auto *queue = static_cast<unsigned char *>(queueMemory->getBytesNoCopy());
  auto *descriptors = reinterpret_cast<Descriptor *>(queue);
  auto *request = reinterpret_cast<Request *>(queue + 512);
  volatile uint8_t *status = queue + 528;
  *request = {operation, 0, operation == opFlush ? 0 : sector};
  *status = 0xff;
  if (operation == opFlush) {
    descriptors[0] = {queuePhysical + 512, sizeof(Request), 1, 1};
    descriptors[1] = {queuePhysical + 528, 1, 2, 0};
  } else {
    descriptors[0] = {queuePhysical + 512, sizeof(Request), 1, 1};
    descriptors[1] = {transferPhysical, bytes, static_cast<uint16_t>(operation == opRead ? 3 : 1), 2};
    descriptors[2] = {queuePhysical + 528, 1, 2, 0};
  }
  auto *available = reinterpret_cast<volatile uint16_t *>(queue + 128);
  available[2 + availableIndex % queueSize] = 0;
  dmaBarrier();
  ++availableIndex;
  available[1] = availableIndex;
  dmaBarrier();
  write32(0x50, 0);
  auto *used = reinterpret_cast<volatile uint16_t *>(queue + 256);
  uint64_t deadline;
  clock_interval_to_deadline(5000, kMillisecondScale, &deadline);
  while (used[1] == usedIndex) {             // a device model that answers later than its notify
    if (mach_absolute_time() >= deadline) {
      reset();
      __atomic_fetch_add(&oneios_hosted_disk_failures, 1, __ATOMIC_RELAXED);
      return kIOReturnTimeout;
    }
    dmaBarrier();
    IODelay(10);
  }
  dmaBarrier();
  auto *element = reinterpret_cast<volatile Used *>(queue + 260) + usedIndex % queueSize;
  bool valid = used[1] == static_cast<uint16_t>(usedIndex + 1) && element->id == 0 && *status == 0 &&
               (operation != opRead || element->length >= bytes + 1);
  ++usedIndex;
  uint32_t interrupts = read32(0x60) & 3;
  if (interrupts) write32(0x64, interrupts);
  if (!valid) {
    // A read the host refuses is a chunk that failed its hash: the disk stays up, the read fails.
    __atomic_fetch_add(&oneios_hosted_disk_failures, 1, __ATOMIC_RELAXED);
    return kIOReturnIOError;
  }
  __atomic_fetch_add(&oneios_hosted_disk_requests, 1, __ATOMIC_RELAXED);
  return kIOReturnSuccess;
}

static void stringProperty(IOService *provider, const char *key, char *out, size_t size)
{
  out[0] = 0;
  OSData *data = OSDynamicCast(OSData, provider->getProperty(key));
  if (!data || !data->getLength()) return;
  size_t length = data->getLength() < size - 1 ? data->getLength() : size - 1;
  memcpy(out, data->getBytesNoCopy(), length);
  out[length] = 0;
}

void OneiOSHostedDisk::identity(IOService *provider)
{
  stringProperty(provider, "oneios,role", role, sizeof(role));
  stringProperty(provider, "oneios,volume-uuid", volumeUUID, sizeof(volumeUUID));
  stringProperty(provider, "oneios,sha256", seedSHA256, sizeof(seedSHA256));
  for (char *c = volumeUUID; *c; ++c) if (*c >= 'a' && *c <= 'f') *c = static_cast<char>(*c - 32);   // as IOMedia's UUID
  OSData *bytes = OSDynamicCast(OSData, provider->getProperty("oneios,bytes"));
  if (bytes && bytes->getLength() == 8) memcpy(&contentBytes, bytes->getBytesNoCopy(), 8);
  if (contentBytes > capacity * 512) contentBytes = 0;
  OSData *policy = OSDynamicCast(OSData, provider->getProperty("oneios,verified"));
  // Only a disk the device itself refuses to write can be a verified one.
  verified = policy && policy->getLength() == sizeof(verifiedDigest) && !writable;
  if (verified) memcpy(verifiedDigest, policy->getBytesNoCopy(), sizeof(verifiedDigest));
  if (role[0]) setProperty("oneios,role", role);
  setProperty("oneios,verified", verified);
}

bool OneiOSHostedDisk::start(IOService *provider)
{
  if (!IOBlockStorageDevice::start(provider)) return false;
  IODeviceMemory *window = provider->getDeviceMemoryWithIndex(0);
  if (!window || window->getLength() < 0x108) return false;
  registers = window->getPhysicalAddress();
  if (read32(0x00) != 0x74726976 || read32(0x04) != 2 || read32(0x08) != 2) { registers = 0; return false; }
  gate = IOLockAlloc();
  if (!gate || !configureQueue()) return false;
  identity(provider);
  IOLockLock(gate);
  IOReturn first = transfer(opRead, 0, 512);
  IOLockUnlock(gate);
  if (first) {
    printf("OneiOSHostedDisk: %s (%s): the first sector cannot be read (%x)\n", provider->getName(), role, first);
    return false;
  }
  unsigned slot = __atomic_fetch_add(&oneios_hosted_disks, 1, __ATOMIC_RELAXED);
  if (slot < maxDisks) disks[slot] = this;
  printf("OneiOSHostedDisk: %s: %llu MiB, role %s%s%s\n", provider->getName(), capacity >> 11, role[0] ? role : "none",
         writable ? ", writable" : ", read-only", verified ? ", verified by xnuhost" : "");
  registerService();
  return true;
}

IOReturn OneiOSHostedDisk::doAsyncReadWrite(IOMemoryDescriptor *buffer, UInt64 block, UInt64 count,
                                            IOStorageAttributes *, IOStorageCompletion *completion)
{
  if (!buffer || !completion || !completion->action || !count || block >= capacity ||
      count > capacity - block || count > UINT64_MAX / 512 || buffer->getLength() < count * 512) return kIOReturnBadArgument;
  const bool reading = buffer->getDirection() == kIODirectionIn;
  if (!reading && buffer->getDirection() != kIODirectionOut) return kIOReturnBadArgument;
  if (!reading && !writable) return kIOReturnNotWritable;
  uint64_t actual = 0;
  IOReturn result = kIOReturnNotReady;
  IOLockLock(gate);
  while (online && actual < count * 512) {
    uint32_t bytes = static_cast<uint32_t>(count * 512 - actual > transferSize ? transferSize : count * 512 - actual);
    if (!reading && buffer->readBytes(actual, transferMemory->getBytesNoCopy(), bytes) != bytes) {
      result = kIOReturnIOError;
      break;
    }
    result = transfer(reading ? opRead : opWrite, block + actual / 512, bytes);
    if (result) break;
    if (reading && buffer->writeBytes(actual, transferMemory->getBytesNoCopy(), bytes) != bytes) {
      result = kIOReturnIOError;
      break;
    }
    actual += bytes;
  }
  IOLockUnlock(gate);
  completion->action(completion->target, completion->parameter, result, actual);
  return kIOReturnSuccess;
}

IOReturn OneiOSHostedDisk::doSynchronizeCache()
{
  if (!online) return kIOReturnNotReady;
  if (!writable || !flushes) return kIOReturnSuccess;
  IOLockLock(gate);
  IOReturn result = transfer(opFlush, 0, 0);
  IOLockUnlock(gate);
  return result;
}

// Zeros at the start of a writable disk, below the storage stack: a Data store that is started over loses
// its container (its superblock and the first checkpoints) and is a blank disk at the next start.
IOReturn OneiOSHostedDisk::zero(uint64_t bytes)
{
  if (!writable) return kIOReturnNotWritable;
  if (bytes > capacity * 512) bytes = capacity * 512;
  bytes &= ~UINT64_C(511);
  IOReturn result = kIOReturnSuccess;
  IOLockLock(gate);
  bzero(transferMemory->getBytesNoCopy(), transferSize);
  for (uint64_t at = 0; at < bytes && !result;) {
    uint32_t part = static_cast<uint32_t>(bytes - at > transferSize ? transferSize : bytes - at);
    result = transfer(opWrite, at / 512, part);
    at += part;
  }
  if (!result && flushes) result = transfer(opFlush, 0, 0);
  IOLockUnlock(gate);
  return result;
}

void OneiOSHostedDisk::stop(IOService *provider)
{
  reset();
  IOBlockStorageDevice::stop(provider);
}

void OneiOSHostedDisk::free()
{
  reset();
  for (unsigned i = 0; i < maxDisks; ++i) if (disks[i] == this) disks[i] = nullptr;
  if (queueMemory) { queueMemory->release(); queueMemory = nullptr; }
  if (transferMemory) { transferMemory->release(); transferMemory = nullptr; }
  if (gate) { IOLockFree(gate); gate = nullptr; }
  IOBlockStorageDevice::free();
}

OneiOSHostedDisk *OneiOSHostedDisk::withRole(const char *wanted)
{
  if (!wanted) return nullptr;
  for (unsigned i = 0; i < maxDisks; ++i) if (disks[i] && !strcmp(disks[i]->role, wanted)) return disks[i];
  return nullptr;
}

// The disk a media object sits on: its container, its partition scheme and the block driver are between them.
OneiOSHostedDisk *OneiOSHostedDisk::ofMedia(IOService *media, unsigned *depth)
{
  unsigned level = 0;
  for (IOService *provider = media; provider && level < 64; provider = provider->getProvider(), ++level) {
    if (OneiOSHostedDisk *disk = OSDynamicCast(OneiOSHostedDisk, provider)) {
      if (depth) *depth = level;
      return disk;
    }
  }
  return nullptr;
}

static OSIterator *allMedia()
{
  OSDictionary *matching = IOService::serviceMatching("IOMedia");
  if (!matching) return nullptr;
  OSIterator *iterator = IOService::getMatchingServices(matching);
  matching->release();
  return iterator;
}

static bool mediaDevice(IOService *media, dev_t *device, char *name, size_t size)
{
  OSString *bsd = OSDynamicCast(OSString, media->getProperty(kIOBSDNameKey));
  OSNumber *mjr = OSDynamicCast(OSNumber, media->getProperty(kIOBSDMajorKey));
  OSNumber *mnr = OSDynamicCast(OSNumber, media->getProperty(kIOBSDMinorKey));
  if (!bsd || !mjr || !mnr || !bsd->getLength() || bsd->getLength() >= size ||
      mjr->unsigned64BitValue() > 255 || mnr->unsigned64BitValue() > 0xffffff) return false;
  *device = static_cast<dev_t>(makedev(mjr->unsigned32BitValue(), mnr->unsigned32BitValue()));
  strlcpy(name, bsd->getCStringNoCopy(), size);
  return true;
}

// The one media object with this device number, and its disk when xnuhost verifies it.
static OneiOSHostedDisk *verifiedDiskOf(dev_t device, const char *expectedUUID)
{
  OSIterator *iterator = allMedia();
  if (!iterator) return nullptr;
  OneiOSHostedDisk *found = nullptr;
  unsigned matches = 0;
  while (OSObject *object = iterator->getNextObject()) {
    IOService *media = OSDynamicCast(IOService, object);
    dev_t candidate = NODEV;
    char name[32];
    if (!media || !mediaDevice(media, &candidate, name, sizeof(name)) || candidate != device) continue;
    if (++matches != 1) { found = nullptr; break; }
    OSString *uuid = OSDynamicCast(OSString, media->getProperty("UUID"));
    if (expectedUUID && (!uuid || strcmp(uuid->getCStringNoCopy(), expectedUUID))) continue;
    OneiOSHostedDisk *disk = OneiOSHostedDisk::ofMedia(media);
    if (disk && disk->isVerified()) found = disk;
  }
  iterator->release();
  return matches == 1 ? found : nullptr;
}

// UniXNU's owner-root questions (bsd_init.c, kern_exec.c), answered from xnuhost's policy.
extern "C" int native_verified_block_root_authorized(dev_t device)
{
  return verifiedDiskOf(device, nullptr) != nullptr;
}

// The identity of a verified volume for the boot manifest: SHA-384 of the hash list's SHA-256 and the volume UUID.
extern "C" int native_verified_block_manifest_sha384(dev_t device, const char *uuid, uint8_t digest[48])
{
  if (!digest) return 0;
  bzero(digest, SHA384_DIGEST_LENGTH);
  if (!uuid || strlen(uuid) != 36) return 0;
  OneiOSHostedDisk *disk = verifiedDiskOf(device, uuid);
  if (!disk) return 0;
  SHA384_CTX context;
  SHA384_Init(&context);
  SHA384_Update(&context, disk->digest(), 32);
  SHA384_Update(&context, uuid, 36);
  SHA384_Final(digest, &context);
  return 1;
}

extern "C" int native_verified_block_volume_device(const char *uuid, dev_t *device, char *path, size_t size)
{
  if (!uuid || !device || !path || size < 16 || strlen(uuid) != 36) return 0;
  *device = NODEV;
  path[0] = 0;
  OSIterator *iterator = allMedia();
  if (!iterator) return 0;
  unsigned matches = 0;
  dev_t candidate = NODEV;
  char name[32] = {};
  bool authenticated = false;
  while (OSObject *object = iterator->getNextObject()) {
    IOService *media = OSDynamicCast(IOService, object);
    OSString *identity = media ? OSDynamicCast(OSString, media->getProperty("UUID")) : nullptr;
    if (!identity || strcmp(identity->getCStringNoCopy(), uuid)) continue;
    if (++matches != 1) { authenticated = false; break; }
    OneiOSHostedDisk *disk = OneiOSHostedDisk::ofMedia(media);
    authenticated = disk && disk->isVerified() && mediaDevice(media, &candidate, name, sizeof(name));
  }
  iterator->release();
  if (!authenticated || matches != 1) return 0;
  *device = candidate;
  snprintf(path, size, "/dev/%s", name);
  return 1;
}

// The APFS volume to mount from the disk of a role ("os", "systemos", "app"), as the image set's manifest names it.
extern "C" int oneios_hosted_volume_uuid(const char *role, char uuid[37])
{
  OneiOSHostedDisk *disk = OneiOSHostedDisk::withRole(role);
  if (!disk || strlen(disk->diskVolumeUUID()) != 36) return 0;
  strlcpy(uuid, disk->diskVolumeUUID(), 37);
  return 1;
}

// The whole disk of a role: its BSD name ("disk4"), the bytes of what it carries and, for the seed, the SHA-256 the
// manifest lists for it.
extern "C" int oneios_hosted_disk_device(const char *role, dev_t *device, char *name, size_t size, uint64_t *bytes, char sha256[65])
{
  OneiOSHostedDisk *disk = OneiOSHostedDisk::withRole(role);
  if (!disk || !device || !name) return 0;
  OSIterator *iterator = allMedia();
  if (!iterator) return 0;
  int found = 0;
  unsigned best = ~0U;
  while (OSObject *object = iterator->getNextObject()) {
    IOService *media = OSDynamicCast(IOService, object);
    unsigned depth = 0;
    dev_t candidate = NODEV;
    char candidateName[32];
    // The media nearest the disk is the whole disk; a container's synthesized disk is further away.
    if (!media || OneiOSHostedDisk::ofMedia(media, &depth) != disk || depth >= best ||
        !mediaDevice(media, &candidate, candidateName, sizeof(candidateName))) continue;
    best = depth;
    *device = candidate;
    strlcpy(name, candidateName, size);
    found = 1;
  }
  iterator->release();
  if (found && bytes) *bytes = disk->diskBytes();
  if (found && sha256) strlcpy(sha256, disk->diskSeedSHA256(), 65);
  return found;
}

// The Data volume: the APFS volume on the disk with the role "data" (the first, if a container has several).
// Not by UUID: the volume is made by the guest's own newfs_apfs on the first start.
extern "C" int oneios_hosted_data_volume(dev_t *device, char *path, size_t size)
{
  OneiOSHostedDisk *disk = OneiOSHostedDisk::withRole("data");
  if (!disk || !device || !path || size < 16) return 0;
  OSIterator *iterator = allMedia();
  if (!iterator) return 0;
  int found = 0;
  char best[32] = {};
  while (OSObject *object = iterator->getNextObject()) {
    IOService *media = OSDynamicCast(IOService, object);
    OSString *content = media ? OSDynamicCast(OSString, media->getProperty("Content")) : nullptr;
    dev_t candidate = NODEV;
    char name[32];
    if (!content || strcmp(content->getCStringNoCopy(), apfsVolumeContent) || OneiOSHostedDisk::ofMedia(media) != disk ||
        !mediaDevice(media, &candidate, name, sizeof(name))) continue;
    if (found && (strlen(name) > strlen(best) || (strlen(name) == strlen(best) && strcmp(name, best) >= 0))) continue;
    strlcpy(best, name, sizeof(best));
    *device = candidate;
    found = 1;
  }
  iterator->release();
  if (found) snprintf(path, size, "/dev/%s", best);
  return found;
}

extern "C" int oneios_hosted_disk_zero(const char *role, uint64_t bytes)
{
  OneiOSHostedDisk *disk = OneiOSHostedDisk::withRole(role);
  return disk && disk->zero(bytes) == kIOReturnSuccess;
}
