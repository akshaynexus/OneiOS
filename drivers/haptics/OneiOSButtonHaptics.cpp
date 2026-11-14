/*
 * SPDX-License-Identifier: MIT
 *
 * OneiOS: a stand-in for the always-on processor's button haptics service, so that iOS 27's ButtonResolver gets
 * ready. Off by default: boot argument oneios-button-haptics=1 (fault 2 of docs/newtodo.md, the power key; its test).
 *
 * Why [read in 24A446's binaries, docs/hardware.md section 5, "The power key: a stand-in for the button haptics
 * service"]:
 *   - backboardd makes BKButtonHapticsController in every boot (start-up step "ButtonHaptics"). Its init enters a
 *     dispatch group that only -[BRButtonResolverController scheduleReadyNotificationOnDispatchQueue:withBlock:]
 *     leaves (log "_BRController ready: device haptic asset slot count:%d"). With home-button-type 2 it queues a block
 *     that waits on that group with no time limit on its serial queue, and every later playHapticForClickState:
 *     (backboardd's MIG routine _BKHIDXXPlayHaptic) does a dispatch_sync on that queue: until ButtonResolver is
 *     ready, its caller never returns.
 *   - ButtonResolver makes BRInterfaceAOP when MobileGestalt's HomeButtonType is 1 or MGIsDeviceOneOfType matches its
 *     list (with fast haptics). That interface is ready only when (a) a service whose registry name is
 *     AppleSPUHapticsAudioDriver (fast haptics; else AppleSPUButtonDriver) is matched (IOServiceNameMatching) and
 *     opened (IOServiceOpen type 0), and (b) Haptics.framework's HAButtonHapticsLoader is available.
 *   - HAButtonHapticsLoader waits for a service with the property "Button Haptics Asset Interface" = 1 (a CFNumber;
 *     IOPropertyMatch), opens it (type 0), maps its memory of type 0, asks scalar method 1 (GetNumberOfSupportedAssets,
 *     one scalar out) and then posts HAButtonHapticsLoaderServiceIsAvailableNotification. Assets go in with struct
 *     method calls (UpdateSSHBAsset) through that memory.
 *   - ButtonResolver plays a state with scalar method 2 (three scalars in) and sets its configuration with
 *     IORegistryEntrySetCFProperty on the service.
 * This guest has no always-on processor: Apple's AppleSPU kexts never match, so neither service exists and
 * ButtonResolver never gets ready. This service is both: registry name AppleSPUHapticsAudioDriver, the property
 * above, a user client that takes every call (selector 1 answers the slot count; selector 2 is counted; the others
 * succeed), memory of type 0, and setProperties that keeps what it is given. Nothing is played on the actuator: an
 * iPhone's side buttons do not click (the Camera Control's do, and the guest has none).
 *
 * Whether this is the power key's cause is not proven (the power key worked before /product/haptics, which none of
 * the above reads). Log: "ONEIOS BUTTON HAPTICS: ..." (the start, each open, the first calls of each selector, each
 * new property key). Boot arguments: oneios-button-haptics=1 (on), oneios-button-haptics-slots=N (selector 1's
 * answer, default 8).
 */
#include <IOKit/IOService.h>
#include <IOKit/IOUserClient.h>
#include <IOKit/IOBufferMemoryDescriptor.h>
#include <IOKit/IOLib.h>
#include <libkern/c++/OSCollectionIterator.h>
#include <libkern/c++/OSDictionary.h>
#include <libkern/c++/OSNumber.h>
#include <libkern/c++/OSString.h>
#include <libkern/c++/OSSymbol.h>
#include <pexpert/pexpert.h>

extern "C" void proc_selfname(char *buf, int size);

#define OBH_NAME        "AppleSPUHapticsAudioDriver"
#define OBH_PROPERTY    "Button Haptics Asset Interface"
#define OBH_MEMORY      (128u * 1024u)  /* the loader's shared memory (type 0); its size is not known: room for waveforms */
#define OBH_SELECTORS   16u
#define OBH_LOG_EACH    4u              /* calls logged per selector */
#define OBH_LOG_KEYS    24u

static uint32_t gObhSlots = 8;
static volatile uint32_t gObhCalls[OBH_SELECTORS + 1];
static volatile uint32_t gObhOpens, gObhKeys;

class OneiOSButtonHaptics : public IOService {
	OSDeclareDefaultStructors(OneiOSButtonHaptics);
public:
	IOBufferMemoryDescriptor *fMemory;
	IOReturn newUserClient(task_t owningTask, void *securityID, UInt32 type, OSDictionary *properties,
	    IOUserClient **handler) APPLE_KEXT_OVERRIDE;
	IOReturn setProperties(OSObject *properties) APPLE_KEXT_OVERRIDE;
};

OSDefineMetaClassAndStructors(OneiOSButtonHaptics, IOService);

class OneiOSButtonHapticsClient : public IOUserClient {
	OSDeclareDefaultStructors(OneiOSButtonHapticsClient);
public:
	OneiOSButtonHaptics *fService;
	IOReturn clientClose(void) APPLE_KEXT_OVERRIDE;
	IOReturn clientMemoryForType(UInt32 type, IOOptionBits *options, IOMemoryDescriptor **memory) APPLE_KEXT_OVERRIDE;
	IOReturn externalMethod(uint32_t selector, IOExternalMethodArguments *args, IOExternalMethodDispatch *dispatch,
	    OSObject *target, void *reference) APPLE_KEXT_OVERRIDE;
};

OSDefineMetaClassAndStructors(OneiOSButtonHapticsClient, IOUserClient);

IOReturn
OneiOSButtonHaptics::newUserClient(task_t owningTask, void *securityID, UInt32 type, OSDictionary *properties,
    IOUserClient **handler)
{
	char who[33] = "";
	proc_selfname(who, sizeof(who));
	OneiOSButtonHapticsClient *client = OSTypeAlloc(OneiOSButtonHapticsClient);
	if (client == nullptr || !client->initWithTask(owningTask, securityID, type, properties)) {
		OSSafeReleaseNULL(client);
		return kIOReturnNoMemory;
	}
	client->fService = this;
	if (!client->attach(this) || !client->start(this)) {
		client->detach(this);
		client->release();
		return kIOReturnError;
	}
	*handler = client;
	uint32_t n = ++gObhOpens;
	if (n <= 16) {
		IOLog("ONEIOS BUTTON HAPTICS: user client %u (type %u) opened by %s\n", n, (unsigned)type, who);
	}
	return kIOReturnSuccess;
}

/* ButtonResolver's configuration (IORegistryEntrySetCFProperty): kept in the registry, every key taken. */
IOReturn
OneiOSButtonHaptics::setProperties(OSObject *properties)
{
	OSDictionary *dict = OSDynamicCast(OSDictionary, properties);
	if (dict == nullptr) {
		return kIOReturnBadArgument;
	}
	char who[33] = "";
	proc_selfname(who, sizeof(who));
	OSCollectionIterator *it = OSCollectionIterator::withCollection(dict);
	if (it == nullptr) {
		return kIOReturnNoMemory;
	}
	while (OSSymbol *key = OSDynamicCast(OSSymbol, it->getNextObject())) {
		if (key->isEqualTo(OBH_PROPERTY)) {
			continue;               /* what the loader matches on stays as published */
		}
		bool known = getProperty(key) != nullptr;
		setProperty(key, dict->getObject(key));
		if (!known && gObhKeys++ < OBH_LOG_KEYS) {
			IOLog("ONEIOS BUTTON HAPTICS: %s sets %s\n", who, key->getCStringNoCopy());
		}
	}
	it->release();
	return kIOReturnSuccess;
}

IOReturn
OneiOSButtonHapticsClient::clientClose(void)
{
	terminate();
	return kIOReturnSuccess;
}

/* type 0: the loader's shared memory (IOConnectMapMemory) */
IOReturn
OneiOSButtonHapticsClient::clientMemoryForType(UInt32 type, IOOptionBits *options, IOMemoryDescriptor **memory)
{
	if (type != 0 || fService == nullptr || fService->fMemory == nullptr) {
		return kIOReturnBadArgument;
	}
	fService->fMemory->retain();
	*memory = fService->fMemory;
	*options = 0;
	return kIOReturnSuccess;
}

IOReturn
OneiOSButtonHapticsClient::externalMethod(uint32_t selector, IOExternalMethodArguments *args, IOExternalMethodDispatch *dispatch,
    OSObject *target, void *reference)
{
	(void)dispatch;
	(void)target;
	(void)reference;
	uint32_t at = selector < OBH_SELECTORS ? selector : OBH_SELECTORS;
	uint32_t n = ++gObhCalls[at];
	if (n <= OBH_LOG_EACH) {
		char who[33] = "";
		proc_selfname(who, sizeof(who));
		IOLog("ONEIOS BUTTON HAPTICS: %s calls selector %u (call %u; %u scalars in, first %llu; %u bytes in; %u scalars out)\n",
		    who, selector, n, args->scalarInputCount, args->scalarInputCount >= 1 ? (unsigned long long)args->scalarInput[0] : 0ull,
		    args->structureInputSize, args->scalarOutputCount);
	}
	/* outputs the caller asked for are zero unless a selector says otherwise */
	for (uint32_t i = 0; i < args->scalarOutputCount; i++) {
		args->scalarOutput[i] = 0;
	}
	if (selector == 1 && args->scalarOutputCount >= 1) {
		args->scalarOutput[0] = gObhSlots;      /* GetNumberOfSupportedAssets */
		args->scalarOutputCount = 1;
	}
	if (args->structureOutput != nullptr && args->structureOutputSize != 0) {
		bzero(args->structureOutput, args->structureOutputSize);
	}
	return kIOReturnSuccess;
}

extern "C" void
oneios_button_haptics_start(void)
{
	static bool tried;
	uint32_t on = 0;
	if (tried) {
		return;
	}
	tried = true;
	(void)PE_parse_boot_argn("oneios-button-haptics", &on, sizeof(on));
	(void)PE_parse_boot_argn("oneios-button-haptics-slots", &gObhSlots, sizeof(gObhSlots));
	if (on == 0) {
		IOLog("ONEIOS BUTTON HAPTICS: off (oneios-button-haptics=1 publishes a stand-in " OBH_NAME
		    " so that ButtonResolver gets ready)\n");
		return;
	}
	IOService *resources = IOService::getResourceService();
	IOService *nub = resources != nullptr ? OSTypeAlloc(IOService) : nullptr;
	if (nub == nullptr || !nub->init()) {
		OSSafeReleaseNULL(nub);
		IOLog("ONEIOS BUTTON HAPTICS: off: no nub\n");
		return;
	}
	nub->setName("oneios-button-haptics");
	if (!nub->attach(resources)) {
		nub->release();
		IOLog("ONEIOS BUTTON HAPTICS: off: no attach to IOResources\n");
		return;
	}
	nub->registerService();
	nub->release();

	OneiOSButtonHaptics *service = OSTypeAlloc(OneiOSButtonHaptics);
	OSNumber *one = OSNumber::withNumber(1ull, 32);
	bool ok = service != nullptr && one != nullptr && service->init();
	if (ok) {
		service->fMemory = IOBufferMemoryDescriptor::inTaskWithOptions(kernel_task,
		    kIODirectionInOut | kIOMemoryKernelUserShared, OBH_MEMORY, page_size);
		ok = service->fMemory != nullptr;
	}
	if (ok) {
		bzero(service->fMemory->getBytesNoCopy(), OBH_MEMORY);
		service->setName(OBH_NAME);
		service->setProperty(OBH_PROPERTY, one);
		service->setProperty("OneiOSNote", "a stand-in for the always-on processor's button haptics (drivers/haptics/OneiOSButtonHaptics.cpp)");
		ok = service->attach(nub);
	}
	if (ok && !service->start(nub)) {
		service->detach(nub);
		ok = false;
	}
	OSSafeReleaseNULL(one);
	if (!ok) {
		if (service != nullptr) {
			OSSafeReleaseNULL(service->fMemory);
		}
		OSSafeReleaseNULL(service);
		IOLog("ONEIOS BUTTON HAPTICS: off: the service could not be made\n");
		return;
	}
	service->registerService();
	service->release();             /* the registry keeps it */
	IOLog("ONEIOS BUTTON HAPTICS: on: " OBH_NAME " published (\"" OBH_PROPERTY "\" = 1, %u asset slots): for ButtonResolver's "
	    "AOP interface and Haptics.framework's HAButtonHapticsLoader; nothing is played\n", gObhSlots);
}
