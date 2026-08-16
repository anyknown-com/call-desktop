//! Preferred ducker on macOS 14.2+: a Core Audio process tap with `CATapMuted` covering every
//! process except our own. While the tap exists and is read by an IOProc on a private aggregate
//! device, the tapped processes' output is silenced at the system level. Apple's docs suggest
//! `CATapMuted` mutes from tap creation regardless of read activity (as opposed to
//! `CATapMutedWhenTapped`); we still run the aggregate device + IOProc because that is the
//! documented, known-working configuration. Set `VOICE_OS_TAP_ONLY=1` to try the tap alone.
//!
//! Only `Mute` is implemented; `DuckMode::Duck` behaves as `Mute` (true attenuation would need us
//! to re-play the tapped audio ourselves, which is out of scope).
//!
//! Requires the *System Audio Recording* TCC permission; the first `duck()` triggers the prompt.
//! Creating a tap on this backend is deferred to `duck()` so the prompt only appears when the
//! feature is actually used; `new()` merely checks the API is present (macOS >= 14.2).

use std::ffi::{c_void, CStr};
use std::mem::size_of;
use std::ptr::NonNull;

use objc2::rc::Retained;
use objc2::AllocAnyThread;
use objc2_core_audio::{
    kAudioAggregateDeviceIsPrivateKey, kAudioAggregateDeviceMainSubDeviceKey,
    kAudioAggregateDeviceNameKey, kAudioAggregateDeviceSubDeviceListKey,
    kAudioAggregateDeviceTapAutoStartKey, kAudioAggregateDeviceTapListKey,
    kAudioAggregateDeviceUIDKey, kAudioDevicePropertyDeviceUID,
    kAudioHardwarePropertyDefaultOutputDevice, kAudioHardwarePropertyTranslatePIDToProcessObject,
    kAudioObjectPropertyElementMain, kAudioObjectPropertyScopeGlobal, kAudioObjectSystemObject,
    kAudioSubDeviceUIDKey, kAudioSubTapUIDKey, AudioDeviceCreateIOProcID,
    AudioDeviceDestroyIOProcID, AudioDeviceIOProcID, AudioDeviceStart, AudioDeviceStop,
    AudioHardwareCreateAggregateDevice, AudioHardwareDestroyAggregateDevice,
    AudioObjectGetPropertyData, AudioObjectID, AudioObjectPropertyAddress, CATapDescription,
    CATapMuteBehavior,
};
use objc2_core_audio_types::{AudioBufferList, AudioTimeStamp};
use objc2_core_foundation::{CFArray, CFDictionary, CFNumber, CFRetained, CFString, CFType};
use objc2_foundation::{NSArray, NSNumber, NSString};

use crate::{DuckMode, Error, MediaDucker, Result};

type OSStatus = i32;
type CreateTapFn =
    unsafe extern "C-unwind" fn(*const CATapDescription, *mut AudioObjectID) -> OSStatus;
type DestroyTapFn = unsafe extern "C-unwind" fn(AudioObjectID) -> OSStatus;

/// The tap API only exists on macOS 14.2+; resolve it dynamically so the binary still loads
/// (and falls back to AppleScript) on older systems.
struct TapApi {
    create: CreateTapFn,
    destroy: DestroyTapFn,
}

impl TapApi {
    fn load() -> Result<Self> {
        unsafe fn sym(name: &CStr) -> Option<*mut c_void> {
            let p = unsafe { libc::dlsym(libc::RTLD_DEFAULT, name.as_ptr()) };
            (!p.is_null()).then_some(p)
        }
        // SAFETY: symbol names and signatures match the CoreAudio headers.
        unsafe {
            let create = sym(c"AudioHardwareCreateProcessTap");
            let destroy = sym(c"AudioHardwareDestroyProcessTap");
            match (create, destroy) {
                (Some(c), Some(d)) => Ok(Self {
                    create: std::mem::transmute::<*mut c_void, CreateTapFn>(c),
                    destroy: std::mem::transmute::<*mut c_void, DestroyTapFn>(d),
                }),
                _ => Err(Error::Unsupported(
                    "Core Audio process taps need macOS 14.2+".into(),
                )),
            }
        }
    }
}

fn os_err(call: &'static str, status: OSStatus) -> Error {
    let b = status.to_be_bytes();
    let fourcc = if b.iter().all(|c| c.is_ascii_graphic() || *c == b' ') {
        String::from_utf8_lossy(&b).into_owned()
    } else {
        String::new()
    };
    let hint = if call == "AudioHardwareCreateProcessTap" {
        "; check the System Audio Recording permission (System Settings > Privacy & Security > Screen & System Audio Recording)"
    } else {
        ""
    };
    Error::CoreAudio {
        call,
        status,
        fourcc,
        hint,
    }
}

fn check(call: &'static str, status: OSStatus) -> Result<()> {
    if status == 0 {
        Ok(())
    } else {
        Err(os_err(call, status))
    }
}

fn address(selector: u32) -> AudioObjectPropertyAddress {
    AudioObjectPropertyAddress {
        mSelector: selector,
        mScope: kAudioObjectPropertyScopeGlobal,
        mElement: kAudioObjectPropertyElementMain,
    }
}

/// `AudioObjectGetPropertyData` for a plain `T` value with an optional qualifier.
///
/// # Safety
/// `T` must be the exact type Core Audio returns for `selector`.
unsafe fn get_property<T: Copy, Q>(
    call: &'static str,
    object: AudioObjectID,
    selector: u32,
    qualifier: Option<&Q>,
    mut out: T,
) -> Result<T> {
    let mut addr = address(selector);
    let mut size = size_of::<T>() as u32;
    let (qsize, qptr) = match qualifier {
        Some(q) => (size_of::<Q>() as u32, q as *const Q as *const c_void),
        None => (0, std::ptr::null()),
    };
    let status = unsafe {
        AudioObjectGetPropertyData(
            object,
            NonNull::from(&mut addr),
            qsize,
            qptr,
            NonNull::from(&mut size),
            NonNull::from(&mut out).cast(),
        )
    };
    check(call, status)?;
    Ok(out)
}

fn own_process_object() -> Result<AudioObjectID> {
    let pid = std::process::id() as libc::pid_t;
    unsafe {
        get_property(
            "TranslatePIDToProcessObject",
            kAudioObjectSystemObject as AudioObjectID,
            kAudioHardwarePropertyTranslatePIDToProcessObject,
            Some(&pid),
            0u32,
        )
    }
}

fn default_output_uid() -> Result<CFRetained<CFString>> {
    unsafe {
        let dev = get_property::<AudioObjectID, ()>(
            "DefaultOutputDevice",
            kAudioObjectSystemObject as AudioObjectID,
            kAudioHardwarePropertyDefaultOutputDevice,
            None,
            0,
        )?;
        let raw = get_property::<*const CFString, ()>(
            "DeviceUID",
            dev,
            kAudioDevicePropertyDeviceUID,
            None,
            std::ptr::null(),
        )?;
        // The property returns a +1 retained CFString.
        NonNull::new(raw.cast_mut())
            .map(|p| CFRetained::from_raw(p))
            .ok_or_else(|| os_err("DeviceUID", -1))
    }
}

unsafe extern "C-unwind" fn discard_io_proc(
    _device: AudioObjectID,
    _now: NonNull<AudioTimeStamp>,
    _input: NonNull<AudioBufferList>,
    _input_time: NonNull<AudioTimeStamp>,
    _output: NonNull<AudioBufferList>,
    _output_time: NonNull<AudioTimeStamp>,
    _client: *mut c_void,
) -> OSStatus {
    0
}

fn cf_str(s: &CStr) -> CFRetained<CFString> {
    CFString::from_str(s.to_str().expect("ascii key"))
}

fn cf_dict(pairs: &[(&CStr, &CFType)]) -> CFRetained<CFDictionary<CFString, CFType>> {
    let keys: Vec<CFRetained<CFString>> = pairs.iter().map(|(k, _)| cf_str(k)).collect();
    let key_refs: Vec<&CFString> = keys.iter().map(|k| &**k).collect();
    let values: Vec<&CFType> = pairs.iter().map(|(_, v)| *v).collect();
    CFDictionary::from_slices(&key_refs, &values)
}

/// A live tap + aggregate device + running IOProc. Dropping/destroying it un-mutes.
struct LiveTap {
    api: TapApi,
    tap: AudioObjectID,
    aggregate: AudioObjectID,
    io_proc: AudioDeviceIOProcID,
}

impl LiveTap {
    fn create(api: TapApi) -> Result<Self> {
        let me = own_process_object()?;
        let output_uid = default_output_uid()?;

        // SAFETY: plain ObjC construction; the description is only used on this thread.
        let (tap, tap_uid) = unsafe {
            let exclude = NSArray::from_retained_slice(&[NSNumber::new_u32(me)]);
            let desc: Retained<CATapDescription> =
                CATapDescription::initStereoGlobalTapButExcludeProcesses(
                    CATapDescription::alloc(),
                    &exclude,
                );
            desc.setName(&NSString::from_str("voice-desktop duck"));
            desc.setMuteBehavior(CATapMuteBehavior::Muted);
            let mut tap: AudioObjectID = 0;
            check(
                "AudioHardwareCreateProcessTap",
                (api.create)(&*desc, &mut tap),
            )?;
            let uid = desc.UUID().UUIDString().to_string();
            (tap, uid)
        };
        let mut live = LiveTap {
            api,
            tap,
            aggregate: 0,
            io_proc: None,
        };
        // Experiment knob: skip the aggregate device + IOProc to check whether `CATapMuted`
        // silences the tapped processes from tap creation alone (see module docs).
        if std::env::var_os("VOICE_OS_TAP_ONLY").is_some() {
            return Ok(live);
        }

        let agg_uid =
            CFString::from_str(&format!("com.anyknown.voice.duck.{}", std::process::id()));
        let name = CFString::from_str("voice-desktop duck");
        let one = CFNumber::new_i32(1);
        let tap_uid = CFString::from_str(&tap_uid);
        let sub_device = cf_dict(&[(kAudioSubDeviceUIDKey, &output_uid)]);
        let sub_devices = CFArray::from_objects(&[&*sub_device]);
        let sub_tap = cf_dict(&[(kAudioSubTapUIDKey, &tap_uid)]);
        let taps = CFArray::from_objects(&[&*sub_tap]);
        let desc = cf_dict(&[
            (kAudioAggregateDeviceUIDKey, &agg_uid),
            (kAudioAggregateDeviceNameKey, &name),
            (kAudioAggregateDeviceIsPrivateKey, &one),
            (kAudioAggregateDeviceMainSubDeviceKey, &output_uid),
            (kAudioAggregateDeviceSubDeviceListKey, &sub_devices),
            (kAudioAggregateDeviceTapAutoStartKey, &one),
            (kAudioAggregateDeviceTapListKey, &taps),
        ]);
        // SAFETY: description dictionary built from valid CF objects; out-pointers are valid.
        unsafe {
            let mut agg: AudioObjectID = 0;
            check(
                "AudioHardwareCreateAggregateDevice",
                AudioHardwareCreateAggregateDevice(desc.as_ref(), NonNull::from(&mut agg)),
            )?;
            live.aggregate = agg;
            let mut proc_id: AudioDeviceIOProcID = None;
            check(
                "AudioDeviceCreateIOProcID",
                AudioDeviceCreateIOProcID(
                    agg,
                    Some(discard_io_proc),
                    std::ptr::null_mut(),
                    NonNull::from(&mut proc_id),
                ),
            )?;
            live.io_proc = proc_id;
            check("AudioDeviceStart", AudioDeviceStart(agg, proc_id))?;
        }
        Ok(live)
    }
}

impl Drop for LiveTap {
    fn drop(&mut self) {
        // SAFETY: tearing down objects we created; errors are ignored on purpose (best effort).
        unsafe {
            if self.io_proc.is_some() {
                AudioDeviceStop(self.aggregate, self.io_proc);
                AudioDeviceDestroyIOProcID(self.aggregate, self.io_proc);
            }
            if self.aggregate != 0 {
                AudioHardwareDestroyAggregateDevice(self.aggregate);
            }
            (self.api.destroy)(self.tap);
        }
    }
}

pub struct ProcessTapDucker {
    live: Option<LiveTap>,
}

impl ProcessTapDucker {
    /// Succeeds only if the process-tap API exists on this macOS. Does not touch audio yet.
    pub fn new() -> Result<Self> {
        TapApi::load()?;
        Ok(Self { live: None })
    }
}

impl MediaDucker for ProcessTapDucker {
    fn duck(&mut self, _mode: DuckMode) -> Result<()> {
        if self.live.is_none() {
            self.live = Some(LiveTap::create(TapApi::load()?)?);
        }
        Ok(())
    }

    fn restore(&mut self) -> Result<()> {
        self.live = None;
        Ok(())
    }

    fn is_ducked(&self) -> bool {
        self.live.is_some()
    }

    fn backend_name(&self) -> &'static str {
        "macos-process-tap"
    }
}
