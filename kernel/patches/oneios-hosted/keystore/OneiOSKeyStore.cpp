// SPDX-License-Identifier: BSD-2-Clause-Patent
// OneiOS, hosted kernel: the AppleKeyStore of a guest with no Secure Enclave.
//
// THIS IS NOT A SECURITY BOUNDARY. The class keys below are derived from a constant in this file. Anything that can
// read the Data store can unwrap every key this service wrapped. It exists so that iOS's daemons find the service
// they expect and the keychain can store items; the Data store's protection is whatever One UI gives the file.
//
// The service and its user client are UniXNU's AppleKeyStoreState (NativeKeyStoreEvents.cpp, BSD-2-Clause-Patent,
// compiled unchanged but for two inserted lines: build-oneios-keystore.py): an IOService of class "AppleKeyStore"
// on IOResources, whose user client answers libaks/MobileKeyBag's state queries for a device with no passcode:
//   - the system keybag is handle 0 and is always loaded (aks_get_system); keybagd needs no systembag.kb;
//   - nothing is ever locked (lock state "unlocked since boot", device state sls = 3, "no passcode").
// This file is OneiOS's side of it, after Droid-VM's package/KeyStoreProbe.cpp and KeyStoreWrap.inc:
//   - oneios_keystore_start(): registers the two classes inside a module load and starts the service
//     (from oneios_board_late_start, 0022-keystore.patch);
//   - the hooks NativeKeyStoreEvents.cpp calls (enabled, lock state, counters, which also log each selector once);
//   - aks_wrap_key (selector 10) and aks_unwrap_key (11), which the component refuses: securityd wraps every
//     keychain item's key with them. Here: one 256-bit key per protection class, and AES key wrap with padding
//     (RFC 5649, so any key length) under that class's key, in software through the kernel's AES (corecrypto).
//     A wrapped key is as long as a real one (the key rounded up to 8 bytes, plus 8). Unwrapping with another
//     class than the one it was wrapped with fails, as on a phone.
// The collection's own AppleSEPKeyStore kext (the real class of this name) is left out of kernel.kc
// (kernel/prelink/roots.py): with it in, this class could not register.
#include <IOKit/IOLib.h>
#include <IOKit/IOMemoryDescriptor.h>
#include <IOKit/IOService.h>
#include <IOKit/IOUserClient.h>
#include <libkern/c++/OSMetaClass.h>
#include <libkern/crypto/aes.h>
#include <libkern/crypto/crypto_internal.h>
#include <mach/kmod.h>
#include <pexpert/pexpert.h>
#include <string.h>

extern kmod_info_t g_kernel_kmod_info;
extern "C" void oneios_late_init_keystore(void) __asm__("__GLOBAL__sub_I_NativeKeyStoreEvents.cpp");
extern "C" IOService *native_keyevents_allocate(void);
extern "C" void proc_selfname(char *buf, int size);

extern "C" {
// 1 classes registered, 2 class keys made and checked, 3 started; 0x80 | n refused at step n; 0x40 switched off.
volatile uint32_t oneios_keystore_stage;
volatile uint32_t oneios_keystore_opens, oneios_keystore_calls, oneios_keystore_refused_opens;
void oneios_keystore_start(void);
IOReturn oneios_keystore_wrap(uint32_t selector, IOExternalMethodArguments *args);
// NativeKeyStoreEvents.cpp's hooks.
void native_keyevents_open_refused(void);
bool native_aks_state_enabled(void);
uint32_t native_aks_lock_state(void);
void native_aks_call(uint32_t selector, int32_t result);
void native_aks_opened(void);
}

namespace {
constexpr unsigned kClasses = 32;              // a class is its low five bits; the bits above are libaks's flags
constexpr unsigned kKeyBytes = 32;
constexpr uint32_t kMaxKey = 512;              // the longest key taken (libaks sends 32 to 64 bytes)
constexpr IOReturn kAksError = (IOReturn)0xe00002bc;
uint8_t gClassKey[kClasses][kKeyBytes];
// Not a secret: see the top of the file.
const uint8_t kRoot[kKeyBytes] = {'O', 'n', 'e', 'i', 'O', 'S', ' ', 'k', 'e', 'y', ' ', 's', 't', 'o', 'r', 'e',
                                  ':', ' ', 'n', 'o', 't', ' ', 'a', ' ', 's', 'e', 'c', 'r', 'e', 't', '.', '1'};
const uint8_t kZeroIV[AES_BLOCK_SIZE] = {};

bool makeClassKeys()
{
    aes_encrypt_ctx root;
    if (!g_crypto_funcs || aes_encrypt_key(kRoot, kKeyBytes, &root)) return false;
    for (unsigned cls = 0; cls < kClasses; ++cls) {
        for (unsigned half = 0; half < 2; ++half) {
            uint8_t block[AES_BLOCK_SIZE] = {'O', 'n', 'e', 'i', 'O', 'S', ' ', 'c', 'l', 'a', 's', 's', ' ', 0, 0, 0};
            block[14] = (uint8_t)half;
            block[15] = (uint8_t)cls;
            if (aes_encrypt_cbc(block, kZeroIV, 1, gClassKey[cls] + half * AES_BLOCK_SIZE, &root)) return false;
        }
    }
    return true;
}

void aiv(uint8_t a[8], uint32_t length)
{
    a[0] = 0xa6; a[1] = 0x59; a[2] = 0x59; a[3] = 0xa6;
    a[4] = (uint8_t)(length >> 24); a[5] = (uint8_t)(length >> 16); a[6] = (uint8_t)(length >> 8); a[7] = (uint8_t)length;
}

// RFC 5649: `length` bytes (1..kMaxKey) to roundup(length, 8) + 8 bytes. `out` holds at least that.
bool wrapKey(const uint8_t *kek, const uint8_t *in, uint32_t length, uint8_t *out, uint32_t *produced)
{
    aes_encrypt_ctx ctx;
    if (aes_encrypt_key(kek, kKeyBytes, &ctx)) return false;
    const uint32_t n = (length + 7) / 8;
    uint8_t a[8], b[AES_BLOCK_SIZE], c[AES_BLOCK_SIZE];
    aiv(a, length);
    memset(out + 8, 0, n * 8);
    memcpy(out + 8, in, length);
    if (n == 1) {
        memcpy(b, a, 8); memcpy(b + 8, out + 8, 8);
        if (aes_encrypt_cbc(b, kZeroIV, 1, out, &ctx)) return false;
    } else {
        for (uint32_t j = 0; j < 6; ++j) {
            for (uint32_t i = 1; i <= n; ++i) {
                memcpy(b, a, 8); memcpy(b + 8, out + 8 * i, 8);
                if (aes_encrypt_cbc(b, kZeroIV, 1, c, &ctx)) return false;
                const uint32_t t = n * j + i;
                memcpy(a, c, 8);
                a[4] ^= (uint8_t)(t >> 24); a[5] ^= (uint8_t)(t >> 16); a[6] ^= (uint8_t)(t >> 8); a[7] ^= (uint8_t)t;
                memcpy(out + 8 * i, c + 8, 8);
            }
        }
        memcpy(out, a, 8);
    }
    *produced = n * 8 + 8;
    return true;
}

// The reverse; false when the bytes were not wrapped with this key (or were changed).
bool unwrapKey(const uint8_t *kek, const uint8_t *in, uint32_t length, uint8_t *out, uint32_t *produced)
{
    aes_decrypt_ctx ctx;
    if (length < 16 || length % 8 || length > kMaxKey + 8 || aes_decrypt_key(kek, kKeyBytes, &ctx)) return false;
    const uint32_t n = length / 8 - 1;
    uint8_t a[8], b[AES_BLOCK_SIZE], c[AES_BLOCK_SIZE], r[kMaxKey];
    if (n == 1) {
        if (aes_decrypt_cbc(in, kZeroIV, 1, c, &ctx)) return false;
        memcpy(a, c, 8); memcpy(r, c + 8, 8);
    } else {
        memcpy(a, in, 8);
        memcpy(r, in + 8, n * 8);
        for (uint32_t j = 6; j-- > 0;) {
            for (uint32_t i = n; i >= 1; --i) {
                const uint32_t t = n * j + i;
                a[4] ^= (uint8_t)(t >> 24); a[5] ^= (uint8_t)(t >> 16); a[6] ^= (uint8_t)(t >> 8); a[7] ^= (uint8_t)t;
                memcpy(b, a, 8); memcpy(b + 8, r + 8 * (i - 1), 8);
                if (aes_decrypt_cbc(b, kZeroIV, 1, c, &ctx)) return false;
                memcpy(a, c, 8); memcpy(r + 8 * (i - 1), c + 8, 8);
            }
        }
    }
    const uint32_t plain = ((uint32_t)a[4] << 24) | ((uint32_t)a[5] << 16) | ((uint32_t)a[6] << 8) | a[7];
    uint8_t expect[8];
    aiv(expect, plain);
    bool ok = memcmp(a, expect, 4) == 0 && plain > (n - 1) * 8 && plain <= n * 8;
    for (uint32_t i = plain; ok && i < n * 8; ++i) ok = r[i] == 0;
    if (ok) { memcpy(out, r, plain); *produced = plain; }
    memset(r, 0, sizeof r);
    return ok;
}

// Wrap, unwrap, a wrong class and a changed byte, with a 32-byte and a 5-byte key.
bool selfTest()
{
    uint8_t key[40], wrapped[48], back[40];
    for (unsigned i = 0; i < sizeof key; ++i) key[i] = (uint8_t)(i * 7 + 1);
    static const uint32_t lengths[] = {32, 5, 40};
    for (uint32_t length : lengths) {
        uint32_t w = 0, p = 0;
        if (!wrapKey(gClassKey[6], key, length, wrapped, &w) || w != (length + 7) / 8 * 8 + 8) return false;
        if (!unwrapKey(gClassKey[6], wrapped, w, back, &p) || p != length || memcmp(back, key, length)) return false;
        if (unwrapKey(gClassKey[7], wrapped, w, back, &p)) return false;
        wrapped[w - 1] ^= 1;
        if (unwrapKey(gClassKey[6], wrapped, w, back, &p)) return false;
    }
    return true;
}

volatile uint32_t gSelectorCount[128];
volatile uint32_t gSelectorLogged[128];        // 1 the first call was logged, 2 its first failure too
}  // namespace

// Selectors 10 (wrap) and 11 (unwrap), as libaks calls them: scalars in {keybag handle, class}, the key in and out
// as structures, and for 10 the class as the one scalar out.
IOReturn oneios_keystore_wrap(uint32_t selector, IOExternalMethodArguments *args)
{
    const bool wrap = selector == 10;
    if (oneios_keystore_stage != 3) return kAksError;
    if (args->scalarInputCount != 2 || !args->scalarInput) return kIOReturnBadArgument;
    if (wrap ? (args->scalarOutputCount < 1 || !args->scalarOutput) : args->scalarOutputCount != 0) return kIOReturnBadArgument;
    uint8_t in[kMaxKey + 8], out[kMaxKey + 8];
    uint32_t length = 0, produced = 0;
    if (args->structureInputDescriptor) {
        const IOByteCount given = args->structureInputDescriptor->getLength();
        if (given > sizeof in || args->structureInputDescriptor->readBytes(0, in, given) != given) return kIOReturnBadArgument;
        length = (uint32_t)given;
    } else {
        length = args->structureInputSize;
        if (length > sizeof in || (length && !args->structureInput)) return kIOReturnBadArgument;
        memcpy(in, args->structureInput, length);
    }
    const uint32_t keyclass = (uint32_t)args->scalarInput[1];
    const uint8_t *kek = gClassKey[keyclass % kClasses];
    IOReturn rc = kIOReturnSuccess;
    if (wrap) {
        if (!length || length > kMaxKey) rc = kIOReturnBadArgument;
        else if (!wrapKey(kek, in, length, out, &produced)) rc = kAksError;
    } else if (!unwrapKey(kek, in, length, out, &produced)) {
        rc = kAksError;
    }
    if (rc == kIOReturnSuccess) {
        if (args->structureOutputDescriptor) {
            if (produced > args->structureOutputDescriptor->getLength()) rc = kIOReturnNoSpace;
            else {
                args->structureOutputDescriptor->writeBytes(0, out, produced);
                args->structureOutputDescriptorSize = produced;
            }
        } else if (args->structureOutput && produced <= args->structureOutputSize) {
            memcpy(args->structureOutput, out, produced);
            args->structureOutputSize = produced;
        } else {
            rc = kIOReturnNoSpace;
        }
    }
    memset(in, 0, sizeof in);
    memset(out, 0, sizeof out);
    if (rc == kIOReturnSuccess && wrap) { args->scalarOutput[0] = keyclass; args->scalarOutputCount = 1; }
    return rc;
}

void native_keyevents_open_refused(void)
{
    oneios_keystore_refused_opens = oneios_keystore_refused_opens + 1;
}

bool native_aks_state_enabled(void)
{
    return oneios_keystore_stage == 3;
}

// What the device state's "sls" says: 3 is MobileKeyBag's "no passcode set" (Droid-VM's iPadOS 27 value);
// oneios-aks-sls=<0..7> overrides it.
uint32_t native_aks_lock_state(void)
{
    static int cached = -1;
    if (cached < 0) {
        uint32_t value = 3;
        if (PE_parse_boot_argn("oneios-aks-sls", &value, sizeof value) && value > 7) value = 3;
        cached = (int)value;
    }
    return (uint32_t)cached;
}

void native_aks_opened(void)
{
    const uint32_t opens = __atomic_add_fetch(&oneios_keystore_opens, 1, __ATOMIC_RELAXED);
    if (opens == 1) {
        char name[32] = "";
        proc_selfname(name, sizeof name);
        IOLog("oneios-keystore: first client: %s\n", name);
    }
}

// Each selector is logged the first time it is called and the first time it fails, with the calling process: the
// boot log then names what the guest asks for that this service does not answer.
void native_aks_call(uint32_t selector, int32_t result)
{
    __atomic_add_fetch(&oneios_keystore_calls, 1, __ATOMIC_RELAXED);
    if (selector >= 128) {
        IOLog("oneios-keystore: selector %u: 0x%x\n", selector, (uint32_t)result);
        return;
    }
    __atomic_add_fetch(&gSelectorCount[selector], 1, __ATOMIC_RELAXED);
    const uint32_t level = result ? 2 : 1;
    const uint32_t before = __atomic_fetch_or(&gSelectorLogged[selector], level, __ATOMIC_RELAXED);
    if (before & level) return;
    char name[32] = "";
    proc_selfname(name, sizeof name);
    IOLog("oneios-keystore: selector %u from %s: 0x%x%s\n", selector, name, (uint32_t)result,
          result == (int32_t)0xe00002c7 ? " (not answered by this service)" : "");
}

void oneios_keystore_start(void)
{
    if (oneios_keystore_stage) return;
    uint32_t enabled = 1;
    PE_parse_boot_argn("oneios-keystore", &enabled, sizeof enabled);
    if (!enabled) {
        oneios_keystore_stage = 0x40;
        return;
    }
    const OSSymbol *name = OSSymbol::withCString("AppleKeyStore");
    const bool taken = name && OSMetaClass::getMetaClassWithName(name);
    OSSafeReleaseNULL(name);
    if (taken) {
        oneios_keystore_stage = 0x81;
        IOLog("oneios-keystore: a class AppleKeyStore exists already (AppleSEPKeyStore is in the kernel collection): "
              "the software key store is not started\n");
        return;
    }
    void *module = OSMetaClass::preModLoad(g_kernel_kmod_info.name);
    if (!module) {
        oneios_keystore_stage = 0x81;
        return;
    }
    oneios_late_init_keystore();
    if (OSMetaClass::postModLoad(module)) {
        oneios_keystore_stage = 0x81;
        IOLog("oneios-keystore: the AppleKeyStore classes did not register\n");
        return;
    }
    oneios_keystore_stage = 1;
    if (!makeClassKeys() || !selfTest()) {
        oneios_keystore_stage = 0x82;
        IOLog("oneios-keystore: the class keys' wrap test failed (the kernel's AES is %s): not started\n",
              g_crypto_funcs ? "registered" : "not registered");
        return;
    }
    oneios_keystore_stage = 2;
    IOService *resources = IOService::getResourceService();
    IOService *service = native_keyevents_allocate();
    if (!service || !resources || !service->init() || !service->attach(resources)) {
        OSSafeReleaseNULL(service);
        oneios_keystore_stage = 0x83;
        IOLog("oneios-keystore: the service could not be made or attached\n");
        return;
    }
    oneios_keystore_stage = 3;                 // before start: the service is registered inside it and may be opened at once
    if (!service->start(resources)) {
        service->detach(resources);
        service->release();
        oneios_keystore_stage = 0x84;
        IOLog("oneios-keystore: the service did not start\n");
        return;
    }
    service->setProperty("OneiOSSoftwareKeyStore", true);
    service->release();
    IOLog("oneios-keystore: AppleKeyStore started in software: system keybag loaded, no passcode, %u class keys, "
          "wrap and unwrap by AES key wrap. Not a security boundary.\n", kClasses - 1);
}
