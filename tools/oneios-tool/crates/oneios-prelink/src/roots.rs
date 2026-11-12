//! `prelink roots KERNELCACHE OUT.json [PROFILE.toml]`: which kexts of the kernelcache go into kernel.kc, and why
//! each one that does not. A port of kernel/prelink/roots.py: the same OUT.json, byte for byte, and the same line
//! on stdout. The rule and the two lists are that script's (kernel/prelink/README.md); when one changes there it
//! must change here (tests/prelink/compare-python.sh compares the two on the real kernelcache).

use anyhow::{anyhow, bail, Context, Result};
use oneios_common::pyjson::{self, Value};
use plist::{Dictionary, Value as Plist};
use std::collections::{BTreeMap, BTreeSet, HashMap};

/// Provider classes that only exist on Apple hardware absent under xnuhost.
const HARDWARE_PROVIDERS: &[&str] = &[
    "AppleARMIODevice", "AppleARMIICDevice", "AppleARMIISDevice", "AppleARMSPIDevice", "AppleARMSPMIDevice",
    "AppleARMNORFlashDevice", "AppleARMPWMDevice", "AppleA7IOPNub", "AppleSEPIntelIOPNub", "AppleSEPDeviceService",
    "AppleSEPManager", "RTBuddy", "RTBuddyService", "RTBuddyEndpointService", "AFKEndpointInterface",
    "AFKMailboxEndpointInterface", "AFKTightbeamEndpoint", "AppleAOP2KextTightbeamEndpoint", "AppleSPUAppInterface",
    "AppleSPUAppDriver", "AppleSPUFastpathInterface", "AppleSPUHIDInterface", "AppleSPUInterface",
    "AppleAOPAudioDeviceNode", "AppleAOPAudioDeviceProvider", "IOPAudioNode", "ApplePMGRNub", "AppleSMC",
    "AppleSMCInterface", "AppleDialogSPMIPMU", "AppleDockChannelDevice", "AppleSmartIODMANub", "AppleNANDConfigAccess",
    "AppleBaseband", "AppleBasebandPCIMHIInterface", "AppleC26DeviceStream", "AppleCSI2CController",
    "AppleCallbackPowerSourceProvider", "AppleDisplayCrossbar", "AppleHIDTransportInterface", "AppleHPMBusDevice",
    "AppleHPMDevice", "AppleHPMDeviceHAL", "AppleTMP103", "AppleAuthCPRelayInterface", "AppleOnboardSerialSync",
    "AppleUVDMEndpoint", "AppleEmbeddedUSBDevice", "AppleT8101USB20XHCILightningPort", "AppleUSBADCAudioNub",
    "AppleUSBDeviceAudioNub", "AppleUSBNCM11Control", "AppleUSBNCMControl", "IOPCIDevice", "IOThunderboltPort",
    "IOThunderboltLocalNode", "IOThunderboltXDomainService", "IOUSBHostDevice", "IOUSBHostInterface", "IOUSBDevice",
    "IOUSBInterface", "IOUSBDeviceController", "IOUSBDeviceInterface", "IOUSBMassStorageDriverNub",
    "IOUSBMassStorageInterfaceNub", "com_apple_driver_AppleUSBCardReaderInterfaceNub", "IOSCSIPeripheralDeviceNub",
    "IOSCSILogicalUnitNub", "IOAccessoryBulkDataEndpoint", "IOAccessoryDock9Pin", "IOAccessoryIDBusBulkDataEndpoint",
    "IOAccessoryIDBusTransport", "IOAccessoryManager", "IOAccessoryManagerAID", "IOAccessoryManagerHSAID",
    "IOAccessoryManagerOrion", "IOPortTransportAIDBus", "IOPortTransportComponentCC", "IOMikeyBusDevice",
    "IOMikeyBusFunctionGroup", "IODPController", "IODPDevice", "IOAVDevice", "IOAVAudioInterface",
    "IOAVSliceVideoInterface", "DCPAVAudioInterfaceProxy", "DCPAVCECInterfaceProxy", "IODARTMapperNub",
    "IONVMeEffaceableDevice", "PSVR2SenseDevice", "IOSerialStreamSync",
];

/// A name-matched IOService or IOPlatformDevice personality attaches to a device-tree node. xnuhost's ADT presents
/// these of the names Apple's kexts match; any other node is absent.
const PRESENTED_NODE_NAMES: &[&str] = &["defaults"];

/// Left out by hand although the rule would keep them (roots.py's FORCE_OUT, word for word).
const FORCE_OUT: &[(&str, &str)] = &[
    ("com.apple.IOTextEncryptionFamily", "not hardware: the FairPlay text crypter. Its start waits 30 s for the service com_apple_driver_FairPlayIOKit (left out, below) inside IOResources' matching, which holds back every driver that waits for the IOBSD resource (the disks' BSD clients, so the root device) for those 30 s"),
    ("com.apple.audio.TrustedClockingKEXT", "the exclave audio clock: its start calls exclaves_coreaudio_register_orchestration_callbacks, a kernel function the public XNU source does not have (no native function to pair it with)"),
    ("com.apple.driver.IOAudioCodecs", "not hardware: its start waits for the service com_apple_driver_FairPlayIOKit, which is left out (below), and holds an IOKit configuration thread forever"),
    ("com.apple.driver.FairPlayIOKit", "not hardware: its code is obfuscated and reaches kernel functions through computed tables, so an unpaired target cannot be named from the call site (its start panics on the stub at 0xfffffe0009be7240). UniXNU keeps it out of its roots as well; userland disables fairplayd.H2 (docs/agents.md)"),
    ("com.apple.kec.pthread", "the kernel links libpthread's kernel part (kernel/README.md); a second pthread registration from the original kext is refused by pthread_kext_register"),
    ("com.apple.driver.AppleSEPKeyStore", "the keystore runs on the SEP, which xnuhost does not present (matched on IOResources, it waits for AppleSEPManager)"),
    ("com.apple.driver.AppleSEPCredentialManager", "a SEP endpoint client (matched on IOResources); no SEP"),
    ("com.apple.security.AKSAnalytics", "analytics of the SEP keystore (AppleSEPKeyStore); no SEP"),
    ("com.apple.driver.AppleT8150ANEHAL", "the T8150 Neural Engine's HAL (matched on IOResources); no ANE"),
];

/// The `__PRELINK_INFO` plist of a kernelcache: its `_PrelinkInfoDictionary` entries.
pub fn prelink_entries(data: &[u8]) -> Result<Vec<Dictionary>> {
    let word = |at: usize| -> Result<u32> {
        data.get(at..at + 4).map(|b| u32::from_le_bytes(b.try_into().unwrap())).ok_or_else(|| anyhow!("the kernelcache ends inside its load commands"))
    };
    let quad = |at: usize| -> Result<u64> {
        data.get(at..at + 8).map(|b| u64::from_le_bytes(b.try_into().unwrap())).ok_or_else(|| anyhow!("the kernelcache ends inside its load commands"))
    };
    let count = word(16)?;
    let mut off = 32usize;
    let mut info = None;
    for _ in 0..count {
        let (cmd, length) = (word(off)?, word(off + 4)? as usize);
        if cmd == 0x19 {
            let name = &data[off + 8..off + 24];
            let name = &name[..name.iter().rposition(|b| *b != 0).map_or(0, |i| i + 1)];
            if name == b"__PRELINK_INFO" {
                let (fileoff, filesize) = (quad(off + 40)? as usize, quad(off + 48)? as usize);
                let raw = data.get(fileoff..fileoff + filesize).ok_or_else(|| anyhow!("__PRELINK_INFO is outside the file"))?;
                let raw = &raw[..raw.iter().position(|b| *b == 0).unwrap_or(raw.len())];
                info = Some(Plist::from_reader_xml(std::io::Cursor::new(raw)).context("__PRELINK_INFO is not a property list")?);
            }
        }
        off += length;
    }
    let info = info.ok_or_else(|| anyhow!("the kernelcache has no __PRELINK_INFO segment"))?;
    let entries = info
        .as_dictionary()
        .and_then(|d| d.get("_PrelinkInfoDictionary"))
        .and_then(Plist::as_array)
        .ok_or_else(|| anyhow!("__PRELINK_INFO has no _PrelinkInfoDictionary"))?;
    entries.iter().map(|e| e.as_dictionary().cloned().ok_or_else(|| anyhow!("a _PrelinkInfoDictionary entry is not a dictionary"))).collect()
}

/// `entry.get(key) or {}` for a dictionary of dictionaries.
fn dict_of<'a>(entry: &'a Dictionary, key: &str) -> Option<&'a Dictionary> {
    entry.get(key).and_then(Plist::as_dictionary)
}

/// Python's truth of a plist value (what `not e.get(...)` tests).
fn truthy(value: Option<&Plist>) -> bool {
    match value {
        None => false,
        Some(Plist::Integer(i)) => i.as_unsigned().map(|v| v != 0).or(i.as_signed().map(|v| v != 0)).unwrap_or(true),
        Some(Plist::Boolean(b)) => *b,
        Some(Plist::String(s)) => !s.is_empty(),
        Some(Plist::Array(a)) => !a.is_empty(),
        Some(Plist::Dictionary(d)) => !d.is_empty(),
        Some(Plist::Data(d)) => !d.is_empty(),
        Some(Plist::Real(r)) => *r != 0.0,
        Some(_) => true,
    }
}

/// `personality.get('IONameMatch', personality.get('compatible'))` as a list of names (`None`: neither key).
fn names_of(personality: &Dictionary) -> Option<Vec<String>> {
    let value = personality.get("IONameMatch").or_else(|| personality.get("compatible"))?;
    Some(match value {
        Plist::String(s) => vec![s.clone()],
        Plist::Array(a) => a.iter().map(|v| v.as_string().unwrap_or_default().to_string()).collect(),
        _ => Vec::new(),
    })
}

fn provider_of<'a>(personality: &'a Dictionary, default: &'a str) -> &'a str {
    personality.get("IOProviderClass").and_then(Plist::as_string).unwrap_or(default)
}

fn personalities_hardware_only(entry: &Dictionary) -> (bool, String) {
    let Some(found) = dict_of(entry, "IOKitPersonalities").filter(|d| !d.is_empty()) else {
        return (false, "no personality".into());
    };
    for personality in found.values().filter_map(Plist::as_dictionary) {
        let provider = provider_of(personality, "");
        if HARDWARE_PROVIDERS.contains(&provider) {
            continue;
        }
        if let (true, Some(names)) = (provider == "IOService" || provider == "IOPlatformDevice", names_of(personality)) {
            if !names.iter().any(|n| PRESENTED_NODE_NAMES.contains(&n.as_str())) {
                continue;
            }
        }
        return (false, format!("a personality on {provider}"));
    }
    (true, "every personality attaches to absent hardware".into())
}

fn describe(entry: &Dictionary) -> Vec<String> {
    let mut out = BTreeSet::new();
    if let Some(found) = dict_of(entry, "IOKitPersonalities") {
        for personality in found.values().filter_map(Plist::as_dictionary) {
            let provider = provider_of(personality, "?");
            match names_of(personality) {
                Some(names) => {
                    let first: Vec<&str> = names.iter().take(3).map(String::as_str).collect();
                    out.insert(format!("{provider}[{}{}]", first.join(","), if names.len() > 3 { ",..." } else { "" }));
                }
                None => {
                    out.insert(provider.to_string());
                }
            }
        }
    }
    out.into_iter().collect()
}

fn libraries(entry: &Dictionary) -> Vec<&str> {
    dict_of(entry, "OSBundleLibraries").map(|d| d.keys().map(String::as_str).collect()).unwrap_or_default()
}

fn strings(items: impl IntoIterator<Item = impl Into<String>>) -> Value {
    Value::List(items.into_iter().map(|s| Value::Str(s.into())).collect())
}

pub fn run(args: &[String]) -> Result<()> {
    if args.len() < 2 || args.len() > 3 {
        bail!("usage: oneios-tool prelink roots KERNELCACHE OUT.json [PROFILE.toml]");
    }
    let (cache_path, out_path) = (&args[0], &args[1]);
    let mut roots_profile: Vec<String> = Vec::new();
    if let Some(profile) = args.get(2) {
        let text = std::fs::read_to_string(profile).with_context(|| profile.clone())?;
        let table: toml::Table = text.parse().with_context(|| profile.clone())?;
        let roots = table.get("kernel").and_then(|k| k.get("prelink")).and_then(|p| p.get("roots")).and_then(|r| r.as_array());
        let roots = roots.ok_or_else(|| anyhow!("{profile}: no [kernel.prelink] roots"))?;
        roots_profile = roots.iter().filter_map(|r| r.as_str().map(str::to_string)).collect();
    }
    let data = oneios_common::Map::open(cache_path)?;
    let entries = prelink_entries(&data)?;
    let ident = |e: &Dictionary| -> Result<String> {
        e.get("CFBundleIdentifier").and_then(Plist::as_string).map(str::to_string).ok_or_else(|| anyhow!("an entry has no CFBundleIdentifier"))
    };
    let mut by_id: HashMap<String, &Dictionary> = HashMap::new();
    for e in &entries {
        by_id.insert(ident(e)?, e);
    }
    let force_out: HashMap<&str, &str> = FORCE_OUT.iter().copied().collect();
    let mut roots: Vec<String> = Vec::new();
    // ident -> (reason, personalities)
    let mut out: BTreeMap<String, (String, Vec<String>)> = BTreeMap::new();
    let mut executables = 0;
    for e in &entries {
        let id = ident(e)?;
        if !truthy(e.get("_PrelinkExecutableLoadAddr")) {
            continue; // codeless entries (KPIs, personality-only bundles) come with what links them
        }
        executables += 1;
        if roots_profile.contains(&id) {
            roots.push(id);
            continue;
        }
        if let Some(reason) = force_out.get(id.as_str()) {
            out.insert(id, (reason.to_string(), describe(e)));
            continue;
        }
        let (hardware, why) = personalities_hardware_only(e);
        if hardware {
            out.insert(id, (why, describe(e)));
        } else {
            roots.push(id);
        }
    }
    let missing: Vec<&String> = {
        let mut m: Vec<&String> = roots_profile.iter().filter(|r| !by_id.contains_key(r.as_str())).collect();
        m.sort();
        m.dedup();
        m
    };
    let mut closure: BTreeSet<String> = BTreeSet::new();
    let mut pending: Vec<String> = roots.clone();
    while let Some(id) = pending.pop() {
        if closure.contains(&id) {
            continue;
        }
        let entry = by_id.get(id.as_str()).ok_or_else(|| anyhow!("{id}: linked against, but not an entry of the kernelcache"))?;
        pending.extend(libraries(entry).into_iter().map(str::to_string));
        closure.insert(id);
    }
    let back: Vec<&String> = closure.iter().filter(|i| out.contains_key(*i)).collect();
    let mut returns: HashMap<&str, Vec<String>> = HashMap::new();
    for i in &back {
        let mut users: Vec<String> = roots.iter().filter(|r| libraries(by_id[r.as_str()]).contains(&i.as_str())).cloned().collect();
        users.sort();
        returns.insert(i.as_str(), users);
    }
    let mut sorted_roots = roots.clone();
    sorted_roots.sort();
    let left_out = Value::Dict(
        out.iter()
            .map(|(id, (reason, personalities))| {
                let mut v = Value::dict();
                v.set("reason", reason.as_str());
                v.set("personalities", strings(personalities.iter().cloned()));
                if let Some(users) = returns.get(id.as_str()) {
                    v.set("returns_as_dependency_of", strings(users.iter().cloned()));
                }
                (id.clone(), v)
            })
            .collect(),
    );
    let mut record = Value::dict();
    record.set("kernelcache_entries", entries.len());
    record.set("kernelcache_executables", executables);
    record.set("profile_roots_missing", strings(missing.iter().map(|s| s.to_string())));
    record.set("roots", strings(sorted_roots));
    record.set("left_out", left_out);
    record.set("left_out_but_linked", strings(back.iter().map(|s| s.to_string())));
    record.set("closure", strings(closure.iter().cloned()));
    // json.dump(record, f, indent=1): no line end after the last brace
    std::fs::write(out_path, pyjson::dumps(&record, Some(1), false)).with_context(|| out_path.clone())?;
    let missing_text = if missing.is_empty() {
        "none".to_string()
    } else {
        format!("[{}]", missing.iter().map(|m| format!("'{m}'")).collect::<Vec<_>>().join(", "))
    };
    println!(
        "roots: {} roots, {} left out ({} of them return as dependencies), {} in the closure; profile roots missing: {missing_text}",
        roots.len(),
        out.len(),
        back.len(),
        closure.len()
    );
    Ok(())
}
