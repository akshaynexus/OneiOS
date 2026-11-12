/*
 * SPDX-License-Identifier: MIT
 *
 * OneiOS camera, IOKit side: the service iOS 27's capture stack looks for, and the way the capture plug-in reads
 * frames from inside cameracaptured.
 *
 * Why this name. CMCapture 27 (24A446) finds its camera driver in fcc_probeCameraDriverService: for each row of
 * its table sCameraDriverInfo it adds an IOServiceMatching(name) first-match notification, waits 5 s, and the row
 * that matched names the plug-in that makes the FigCaptureDevice:
 *
 *   AppleCamera    /System/Library/MediaCapture/ISP.mediacapture      ISPCaptureDeviceCreate
 *   AppleH16CamIn  /System/Library/MediaCapture/H16ISP.mediacapture   H16ISPCaptureDeviceCreate   (in the shared cache)
 *   AppleH13CamIn  ... H13ISP ...   AppleH10CamIn ... H10ISP ...   AppleH9CamIn ... H9ISP ...   AppleH6CamIn ... H6ISP ...
 *
 * FigCaptureGetCaptureDeviceCreateFunction then dlopen()s that path and dlsym()s that symbol, and
 * BWFigCaptureDeviceVendor creates every device with it. Read in CMCapture of the 24A446 cache
 * (___fcc_probeCameraDriverService_block_invoke, _FigCaptureGetCaptureDeviceCreateFunction). Without any of the
 * six the phone's log says "Camera driver not started in time", then "Failed to load CaptureDeviceCreate function",
 * and every client gets error -12786 ("Cannot create device without create function!").
 *
 * AppleH9CamIn is the row to answer: no kext of the collection defines that class (the name is only in the sandbox
 * profiles' lists), so it cannot collide with Apple's; and H9ISP.mediacapture is not in the shared cache, so dyld
 * loads it from /System/Library/MediaCapture, which on the OS volume is an empty folder. The hosted kernel shows
 * the Data folder /private/var/oneios/addons/MediaCapture over it (kernel/patches/oneios-hosted/0049), and the image
 * places the capture plug-in there as H9ISP.mediacapture (userland/camera). AppleCamera is left alone: the ANE
 * driver and mobilerepaird look for that name.
 *
 * The user client. cameracaptured runs in a sandbox. Its profile lists AppleH9CamInUserClient among the user
 * clients it may open (the kernel collection's profile strings: "cameracaptured ... AppleH9CamInUserClient
 * AppleH9CamIn"); a /dev node is not known to be allowed. So the plug-in reads frames through this user client,
 * with the same session as /dev/oneios-camera (oneios_camera_dev.c): one client at a time.
 *
 *   selector 0  INFO     out: struct oneios_camera_info (the host's cameras)
 *   selector 1  COMMAND  in: struct oneios_camera_command (oneios_camera_abi.h): which camera, what size
 *   selector 2  READ     in: scalar 0 = 1 for "do not wait"; out: the frame as the node gives it (header, then the
 *                        picture) into the caller's structure buffer; scalar 0 = bytes given
 *
 * IOService:/IOResources/oneios-camera/AppleH9CamIn, properties OneiOSCameras (the host's count) and OneiOSNote.
 * Boot argument oneios-camera-plugin=0: no AppleH9CamIn (the probe finds nothing, as before; /dev/oneios-camera stays).
 */
#include <IOKit/IOService.h>
#include <IOKit/IOUserClient.h>
#include <IOKit/IOMemoryDescriptor.h>
#include <IOKit/IOLib.h>
#include <libkern/c++/OSNumber.h>
#include <libkern/c++/OSString.h>
#include <pexpert/pexpert.h>
#include <sys/errno.h>

extern "C" void proc_selfname(char *buf, int size);

#include "oneios_camera.h"

struct oneios_camera_info {
	uint32_t count;
	uint32_t reserved;
	struct oc_camera cameras[OC_MAX_CAMERAS];
};
static_assert(sizeof(struct oneios_camera_info) == 8 + 32 * OC_MAX_CAMERAS, "the plug-in's copy of this layout");

class AppleH9CamIn : public IOService {
	OSDeclareDefaultStructors(AppleH9CamIn);
public:
	IOReturn newUserClient(task_t owningTask, void *securityID, UInt32 type, OSDictionary *properties,
	    IOUserClient **handler) APPLE_KEXT_OVERRIDE;
};

OSDefineMetaClassAndStructors(AppleH9CamIn, IOService);

class AppleH9CamInUserClient : public IOUserClient {
	OSDeclareDefaultStructors(AppleH9CamInUserClient);
	bool fOpen;
public:
	bool initWithTask(task_t owningTask, void *securityID, UInt32 type, OSDictionary *properties) APPLE_KEXT_OVERRIDE;
	IOReturn clientClose(void) APPLE_KEXT_OVERRIDE;
	IOReturn externalMethod(uint32_t selector, IOExternalMethodArguments *args, IOExternalMethodDispatch *dispatch,
	    OSObject *target, void *reference) APPLE_KEXT_OVERRIDE;
};

OSDefineMetaClassAndStructors(AppleH9CamInUserClient, IOUserClient);

static uint32_t gClientLines;

static IOReturn
errno_to_ioreturn(int e)
{
	switch (e) {
	case 0: return kIOReturnSuccess;
	case EAGAIN: return kIOReturnTimeout;
	case EBUSY: return kIOReturnBusy;
	case ENXIO: return kIOReturnNotAttached;
	case ENODEV: return kIOReturnNoDevice;
	case EINVAL: return kIOReturnBadArgument;
	case EINTR: return kIOReturnAborted;
	default: return kIOReturnIOError;
	}
}

IOReturn
AppleH9CamIn::newUserClient(task_t owningTask, void *securityID, UInt32 type, OSDictionary *properties, IOUserClient **handler)
{
	char name[33] = "";
	proc_selfname(name, sizeof(name));
	if (type != 0) {
		return kIOReturnBadArgument;
	}
	AppleH9CamInUserClient *client = OSTypeAlloc(AppleH9CamInUserClient);
	if (client == nullptr || !client->initWithTask(owningTask, securityID, type, properties)) {
		OSSafeReleaseNULL(client);
		return kIOReturnNoMemory;
	}
	if (!client->attach(this) || !client->start(this)) {
		client->detach(this);
		client->release();
		return kIOReturnError;
	}
	*handler = client;
	if (gClientLines++ < 20) {
		IOLog("ONEIOS CAMERA: user client opened by %s\n", name);
	}
	return kIOReturnSuccess;
}

bool
AppleH9CamInUserClient::initWithTask(task_t owningTask, void *securityID, UInt32 type, OSDictionary *properties)
{
	fOpen = false;
	return IOUserClient::initWithTask(owningTask, securityID, type, properties);
}

IOReturn
AppleH9CamInUserClient::clientClose(void)
{
	if (fOpen) {
		fOpen = false;
		oneios_camera_session_close();
	}
	terminate();
	return kIOReturnSuccess;
}

/* The frame into the caller's structure buffer: a descriptor for a big one (always, for a picture), else inline. */
struct ocs_out {
	IOMemoryDescriptor *descriptor;
	uint8_t *inline_bytes;
	size_t room;
};

static int
ocs_copy(void *ctx, const void *src, size_t bytes, size_t at)
{
	struct ocs_out *out = static_cast<struct ocs_out *>(ctx);
	if (at > out->room || bytes > out->room - at) {
		return EINVAL;
	}
	if (out->descriptor != nullptr) {
		return out->descriptor->writeBytes(at, src, bytes) == bytes ? 0 : EFAULT;
	}
	memcpy(out->inline_bytes + at, src, bytes);
	return 0;
}

IOReturn
AppleH9CamInUserClient::externalMethod(uint32_t selector, IOExternalMethodArguments *args, IOExternalMethodDispatch *dispatch,
    OSObject *target, void *reference)
{
	(void)dispatch;
	(void)target;
	(void)reference;
	if (!fOpen) {
		int e = oneios_camera_session_open();
		if (e) {
			return errno_to_ioreturn(e);
		}
		fOpen = true;
	}
	switch (selector) {
	case 0: {
		struct oneios_camera_info info = {};
		if (args->structureOutputDescriptor != nullptr || args->structureOutputSize < sizeof(info)) {
			return kIOReturnBadArgument;
		}
		info.count = oneios_camera_session_cameras(info.cameras, OC_MAX_CAMERAS);
		memcpy(args->structureOutput, &info, sizeof(info));
		args->structureOutputSize = sizeof(info);
		return kIOReturnSuccess;
	}
	case 1: {
		struct oneios_camera_command command;
		if (args->structureInputDescriptor != nullptr || args->structureInputSize != sizeof(command)) {
			return kIOReturnBadArgument;
		}
		memcpy(&command, args->structureInput, sizeof(command));
		return errno_to_ioreturn(oneios_camera_session_command(&command));
	}
	case 2: {
		struct ocs_out out = {};
		size_t given = 0;
		bool nowait = args->scalarInputCount >= 1 && args->scalarInput[0] != 0;
		IOReturn r;
		if (args->scalarOutputCount < 1) {
			return kIOReturnBadArgument;
		}
		if (args->structureOutputDescriptor != nullptr) {
			out.descriptor = args->structureOutputDescriptor;
			out.room = (size_t)out.descriptor->getLength();
			if ((r = out.descriptor->prepare(kIODirectionIn)) != kIOReturnSuccess) {
				return r;
			}
		} else {
			out.inline_bytes = static_cast<uint8_t *>(args->structureOutput);
			out.room = args->structureOutputSize;
		}
		int e = oneios_camera_session_read(ocs_copy, &out, out.room, nowait, &given);
		if (out.descriptor != nullptr) {
			out.descriptor->complete(kIODirectionIn);
		} else {
			args->structureOutputSize = (uint32_t)given;
		}
		args->scalarOutput[0] = given;
		args->scalarOutputCount = 1;
		return errno_to_ioreturn(e);
	}
	default:
		return kIOReturnUnsupported;
	}
}

extern "C" void
oneios_camera_service_start(uint32_t cameras)
{
	static bool tried;
	uint32_t on = 1;
	if (tried) {
		return;
	}
	tried = true;
	(void)PE_parse_boot_argn("oneios-camera-plugin", &on, sizeof(on));
	if (on == 0) {
		IOLog("ONEIOS CAMERA: no AppleH9CamIn (oneios-camera-plugin=0): iOS's capture stack finds no camera driver\n");
		return;
	}
	IOService *resources = IOService::getResourceService();
	IOService *nub = resources != nullptr ? OSTypeAlloc(IOService) : nullptr;
	if (nub == nullptr || !nub->init()) {
		OSSafeReleaseNULL(nub);
		IOLog("ONEIOS CAMERA: no AppleH9CamIn: no nub\n");
		return;
	}
	nub->setName("oneios-camera");
	if (!nub->attach(resources)) {
		nub->release();
		IOLog("ONEIOS CAMERA: no AppleH9CamIn: no attach to IOResources\n");
		return;
	}
	nub->registerService();
	nub->release();

	AppleH9CamIn *service = OSTypeAlloc(AppleH9CamIn);
	if (service == nullptr || !service->init()) {
		OSSafeReleaseNULL(service);
		IOLog("ONEIOS CAMERA: no AppleH9CamIn: the service could not be made\n");
		return;
	}
	if (!service->attach(nub) || !service->start(nub)) {
		service->release();
		IOLog("ONEIOS CAMERA: no AppleH9CamIn: the service did not start\n");
		return;
	}
	service->setProperty("OneiOSCameras", (unsigned long long)cameras, 32);
	service->setProperty("OneiOSNote", "the phone's cameras through xnuhost; CMCapture loads "
	    "/System/Library/MediaCapture/H9ISP.mediacapture for this class");
	service->registerService();
	service->release();                     /* the registry keeps it */
	IOLog("ONEIOS CAMERA: AppleH9CamIn published: CMCapture's probe loads /System/Library/MediaCapture/H9ISP.mediacapture\n");
}
