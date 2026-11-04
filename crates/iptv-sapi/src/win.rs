//! Windows SAPI through raw COM IDispatch, mirroring the Python
//! client's ctypes implementation in `app_tts.py` vtable-for-
//! vtable: CoCreateInstance the SpVoice coclass as IDispatch,
//! GetIDsOfNames + Invoke for everything, BSTR/I4/DISPATCH
//! variants only. A failed call is an error value here, never a
//! panic — SAPI problems must degrade to "unavailable", not take
//! the app down.

use std::ffi::c_void;
use std::ptr;

#[repr(C)]
#[derive(Clone, Copy)]
struct Guid {
    data1: u32,
    data2: u16,
    data3: u16,
    data4: [u8; 8],
}

#[repr(C)]
union VariantData {
    l_val: i64,
    bstr: *mut u16,
    pdisp: *mut c_void,
    record: [*mut c_void; 2],
}

#[repr(C)]
struct Variant {
    vt: u16,
    w1: u16,
    w2: u16,
    w3: u16,
    data: VariantData,
}

#[repr(C)]
struct DispParams {
    rgvarg: *mut Variant,
    rgdispid_named_args: *mut i32,
    c_args: u32,
    c_named_args: u32,
}

const VT_I4: u16 = 3;
const VT_BSTR: u16 = 8;
const VT_DISPATCH: u16 = 9;

const DISPATCH_METHOD: u16 = 1;
const DISPATCH_PROPERTYGET: u16 = 2;
const DISPATCH_PROPERTYPUT: u16 = 4;
const DISPATCH_PROPERTYPUTREF: u16 = 8;
const DISPID_PROPERTYPUT: i32 = -3;
const LOCALE_SYSTEM_DEFAULT: u32 = 0x800;
/// SPF_ASYNC | SPF_PURGEBEFORESPEAK | SPF_IS_NOT_XML.
const SPEAK_FLAGS: i32 = 1 | 2 | 16;

type FnAddRef = unsafe extern "system" fn(*mut c_void) -> u32;
type FnGetIdsOfNames = unsafe extern "system" fn(
    *mut c_void,
    *const Guid,
    *const *const u16,
    u32,
    u32,
    *mut i32,
) -> i32;
type FnInvoke = unsafe extern "system" fn(
    *mut c_void,
    i32,
    *const Guid,
    u32,
    u16,
    *mut DispParams,
    *mut Variant,
    *mut c_void,
    *mut u32,
) -> i32;

extern "system" {
    fn CoInitializeEx(reserved: *const c_void, coinit: u32) -> i32;
    fn CoUninitialize();
    fn CLSIDFromString(text: *const u16, guid: *mut Guid) -> i32;
    fn CoCreateInstance(
        clsid: *const Guid,
        outer: *mut c_void,
        clsctx: u32,
        iid: *const Guid,
        ppv: *mut *mut c_void,
    ) -> i32;
    fn SysAllocString(text: *const u16) -> *mut u16;
    fn SysStringLen(bstr: *mut u16) -> u32;
    fn VariantClear(variant: *mut Variant) -> i32;
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

fn guid_from(text: &str) -> Result<Guid, ()> {
    let text = wide(text);
    let mut guid = Guid {
        data1: 0,
        data2: 0,
        data3: 0,
        data4: [0; 8],
    };
    // SAFETY: both pointers are valid for the call.
    let hr = unsafe { CLSIDFromString(text.as_ptr(), &mut guid) };
    if hr == 0 {
        Ok(guid)
    } else {
        Err(())
    }
}

impl Variant {
    fn empty() -> Self {
        Self {
            vt: 0,
            w1: 0,
            w2: 0,
            w3: 0,
            data: VariantData { l_val: 0 },
        }
    }

    fn i4(value: i32) -> Self {
        Self {
            vt: VT_I4,
            ..Self::empty_with_data(VariantData {
                l_val: i64::from(value),
            })
        }
    }

    fn bstr(text: &str) -> Self {
        let text = wide(text);
        // SAFETY: SysAllocString copies the string; ownership of
        // the BSTR passes to the variant (cleared with it).
        let bstr = unsafe { SysAllocString(text.as_ptr()) };
        Self {
            vt: VT_BSTR,
            ..Self::empty_with_data(VariantData { bstr })
        }
    }

    fn dispatch(punk: *mut c_void) -> Self {
        Self {
            vt: VT_DISPATCH,
            ..Self::empty_with_data(VariantData { pdisp: punk })
        }
    }

    fn empty_with_data(data: VariantData) -> Self {
        Self {
            vt: 0,
            w1: 0,
            w2: 0,
            w3: 0,
            data,
        }
    }

    fn clear(&mut self) {
        // SAFETY: variants here own their payloads.
        unsafe {
            VariantClear(self);
        }
    }

    fn as_i4(&self) -> Option<i32> {
        if self.vt == VT_I4 {
            // SAFETY: vt says the payload is an I4 in the low bits.
            Some(unsafe { self.data.l_val } as i32)
        } else {
            None
        }
    }

    fn as_string(&self) -> Option<String> {
        if self.vt != VT_BSTR {
            return None;
        }
        // SAFETY: vt says the payload is a BSTR.
        let bstr = unsafe { self.data.bstr };
        if bstr.is_null() {
            return Some(String::new());
        }
        // SAFETY: BSTRs carry their length; the buffer is valid.
        unsafe {
            let len = SysStringLen(bstr) as usize;
            let slice = std::slice::from_raw_parts(bstr, len);
            Some(String::from_utf16_lossy(slice))
        }
    }

    /// Take ownership of a returned dispatch pointer (the
    /// variant's reference transfers; the variant is emptied so
    /// clearing it cannot release the pointer).
    fn take_dispatch(&mut self) -> Option<*mut c_void> {
        if self.vt != VT_DISPATCH {
            return None;
        }
        // SAFETY: vt says the payload is a dispatch pointer.
        let punk = unsafe { self.data.pdisp };
        self.vt = 0;
        self.data = VariantData { l_val: 0 };
        Some(punk)
    }
}

/// A minimal owned IDispatch client.
struct Dispatch {
    punk: *mut c_void,
}

impl Dispatch {
    /// Wrap an owned dispatch pointer (no AddRef: ownership of
    /// the reference transfers, as from CoCreateInstance/Invoke).
    fn adopt(punk: *mut c_void) -> Self {
        Self { punk }
    }

    fn vtable(&self, index: usize) -> *const c_void {
        // SAFETY: punk is a live COM object; its first word is
        // the vtable pointer, entries are function pointers.
        unsafe {
            let vtbl = *(self.punk as *const *const *const c_void);
            *vtbl.add(index)
        }
    }

    fn release(self) {
        // SAFETY: punk is owned by this wrapper; Release is
        // vtable slot 2.
        unsafe {
            let f: FnAddRef = std::mem::transmute(self.vtable(2));
            f(self.punk);
        }
    }

    fn dispid(&self, name: &str) -> Result<i32, ()> {
        let name = wide(name);
        let names: [*const u16; 1] = [name.as_ptr()];
        let iid_null = Guid {
            data1: 0,
            data2: 0,
            data3: 0,
            data4: [0; 8],
        };
        let mut id: i32 = 0;
        // SAFETY: GetIDsOfNames is vtable slot 5; all pointers
        // are valid for the call.
        let hr = unsafe {
            let f: FnGetIdsOfNames = std::mem::transmute(self.vtable(5));
            f(
                self.punk,
                &iid_null,
                names.as_ptr(),
                1,
                LOCALE_SYSTEM_DEFAULT,
                &mut id,
            )
        };
        if hr == 0 {
            Ok(id)
        } else {
            Err(())
        }
    }

    fn invoke(&self, name: &str, args: &mut [Variant], kind: u16) -> Result<Variant, ()> {
        let dispid = self.dispid(name)?;
        // DISPPARAMS carries arguments right-to-left.
        let mut reversed: Vec<Variant> = Vec::with_capacity(args.len());
        for arg in args.iter_mut() {
            reversed.push(Variant {
                vt: arg.vt,
                w1: 0,
                w2: 0,
                w3: 0,
                data: VariantData {
                    l_val: unsafe { arg.data.l_val },
                },
            });
            arg.vt = 0; // ownership moved into the reversed copy
        }
        reversed.reverse();
        let mut named = [DISPID_PROPERTYPUT];
        let is_put = kind == DISPATCH_PROPERTYPUT || kind == DISPATCH_PROPERTYPUTREF;
        let mut params = DispParams {
            rgvarg: if reversed.is_empty() {
                ptr::null_mut()
            } else {
                reversed.as_mut_ptr()
            },
            rgdispid_named_args: if is_put {
                named.as_mut_ptr()
            } else {
                ptr::null_mut()
            },
            c_args: u32::try_from(reversed.len()).unwrap_or(0),
            c_named_args: u32::from(is_put),
        };
        let mut result = Variant::empty();
        let iid_null = Guid {
            data1: 0,
            data2: 0,
            data3: 0,
            data4: [0; 8],
        };
        // SAFETY: Invoke is vtable slot 6; params/result are
        // valid for the call.
        let hr = unsafe {
            let f: FnInvoke = std::mem::transmute(self.vtable(6));
            f(
                self.punk,
                dispid,
                &iid_null,
                LOCALE_SYSTEM_DEFAULT,
                kind,
                &mut params,
                &mut result,
                ptr::null_mut(),
                ptr::null_mut(),
            )
        };
        for arg in &mut reversed {
            arg.clear();
        }
        if hr != 0 {
            result.clear();
            return Err(());
        }
        Ok(result)
    }

    fn call(&self, name: &str, args: &mut [Variant]) -> Result<Variant, ()> {
        let flags = if args.is_empty() {
            DISPATCH_METHOD | DISPATCH_PROPERTYGET
        } else {
            DISPATCH_METHOD
        };
        self.invoke(name, args, flags)
    }

    fn get(&self, name: &str) -> Result<Variant, ()> {
        self.invoke(name, &mut [], DISPATCH_PROPERTYGET)
    }

    fn put_i4(&self, name: &str, value: i32) -> Result<(), ()> {
        self.invoke(name, &mut [Variant::i4(value)], DISPATCH_PROPERTYPUT)
            .map(|mut result| result.clear())
    }

    fn put_dispatch(&self, name: &str, punk: *mut c_void) -> Result<(), ()> {
        self.invoke(
            name,
            &mut [Variant::dispatch(punk)],
            DISPATCH_PROPERTYPUTREF,
        )
        .map(|mut result| result.clear())
    }
}

/// The SAPI voice itself.
pub struct Voice {
    voice: Option<Dispatch>,
    com_initialized: bool,
    /// What AudioOutput is currently applied: None = never set,
    /// Some(None) = system default, Some(Some(id)) = that token.
    applied: Option<Option<String>>,
}

impl Voice {
    /// Create-and-release probe: true when SAPI answers.
    pub fn probe() -> bool {
        let mut voice = Self::new();
        let ok = voice.available();
        voice.release();
        ok
    }

    pub fn new() -> Self {
        // SAFETY: apartment-threaded init on the owning (UI)
        // thread; S_OK and S_FALSE both mean initialized.
        let hr = unsafe { CoInitializeEx(ptr::null(), 0x2) };
        let com_initialized = hr == 0 || hr == 1;
        let voice = (|| -> Result<Dispatch, ()> {
            let clsid = guid_from("{96749377-3391-11D2-9EE3-00C04F797396}")?;
            let iid = guid_from("{00020400-0000-0000-C000-000000000046}")?;
            let mut ppv: *mut c_void = ptr::null_mut();
            // SAFETY: CLSCTX_ALL (23); ppv receives the interface.
            let hr = unsafe { CoCreateInstance(&clsid, ptr::null_mut(), 23, &iid, &mut ppv) };
            if hr != 0 || ppv.is_null() {
                return Err(());
            }
            Ok(Dispatch::adopt(ppv))
        })();
        Self {
            voice: voice.ok(),
            com_initialized,
            applied: None,
        }
    }

    pub fn available(&self) -> bool {
        self.voice.is_some()
    }

    pub fn speak_text(&mut self, text: &str) -> Result<(), ()> {
        let Some(voice) = &self.voice else {
            return Err(());
        };
        voice
            .call(
                "Speak",
                &mut [Variant::bstr(text), Variant::i4(SPEAK_FLAGS)],
            )
            .map(|mut result| result.clear())
    }

    pub fn purge(&mut self) {
        if let Some(voice) = &self.voice {
            let _ = voice.call("Speak", &mut [Variant::bstr(""), Variant::i4(SPEAK_FLAGS)]);
        }
    }

    pub fn set_volume(&mut self, level: i32) -> bool {
        match &self.voice {
            Some(voice) => voice.put_i4("Volume", level).is_ok(),
            None => false,
        }
    }

    pub fn set_rate(&mut self, rate: i32) -> bool {
        match &self.voice {
            Some(voice) => voice.put_i4("Rate", rate).is_ok(),
            None => false,
        }
    }

    /// The GetAudioOutputs collection, adopted.
    fn outputs_collection(&self) -> Option<Dispatch> {
        let voice = self.voice.as_ref()?;
        let mut result = voice.call("GetAudioOutputs", &mut []).ok()?;
        result.take_dispatch().map(Dispatch::adopt)
    }

    pub fn outputs(&mut self) -> Vec<(String, String)> {
        let Some(tokens) = self.outputs_collection() else {
            return Vec::new();
        };
        let mut out = Vec::new();
        let count = tokens
            .get("Count")
            .ok()
            .and_then(|mut v| {
                let value = v.as_i4();
                v.clear();
                value
            })
            .unwrap_or(0);
        for i in 0..count {
            let item = tokens
                .call("Item", &mut [Variant::i4(i)])
                .ok()
                .and_then(|mut v| v.take_dispatch().map(Dispatch::adopt));
            if let Some(item) = item {
                let id = item.get("Id").ok().and_then(|mut v| {
                    let value = v.as_string();
                    v.clear();
                    value
                });
                let name = item.call("GetDescription", &mut []).ok().and_then(|mut v| {
                    let value = v.as_string();
                    v.clear();
                    value
                });
                if let Some(id) = id.filter(|id| !id.is_empty()) {
                    out.push((id.clone(), name.unwrap_or(id)));
                }
                item.release();
            }
        }
        tokens.release();
        out
    }

    fn find_token(&self, id: &str) -> Option<Dispatch> {
        let tokens = self.outputs_collection()?;
        let count = tokens
            .get("Count")
            .ok()
            .and_then(|mut v| {
                let value = v.as_i4();
                v.clear();
                value
            })
            .unwrap_or(0);
        let mut found = None;
        for i in 0..count {
            let item = tokens
                .call("Item", &mut [Variant::i4(i)])
                .ok()
                .and_then(|mut v| v.take_dispatch().map(Dispatch::adopt));
            if let Some(item) = item {
                let item_id = item.get("Id").ok().and_then(|mut v| {
                    let value = v.as_string();
                    v.clear();
                    value
                });
                if item_id.as_deref() == Some(id) {
                    found = Some(item);
                    break;
                }
                item.release();
            }
        }
        tokens.release();
        found
    }

    pub fn apply_output(&mut self, target: Option<&str>) -> Result<(), ()> {
        let wanted: Option<Option<String>> = Some(target.map(str::to_string));
        if self.applied == wanted {
            return Ok(());
        }
        let Some(voice) = &self.voice else {
            return Err(());
        };
        match target {
            Some(id) => {
                let token = self.find_token(id).ok_or(())?;
                let result = voice.put_dispatch("AudioOutput", token.punk);
                token.release();
                result?;
            }
            None => {
                // Nothing: a null IDispatch (PUTREF).
                voice.put_dispatch("AudioOutput", ptr::null_mut())?;
            }
        }
        self.applied = wanted;
        Ok(())
    }

    pub fn release(&mut self) {
        if let Some(voice) = self.voice.take() {
            voice.release();
        }
        if self.com_initialized {
            self.com_initialized = false;
            // SAFETY: balanced with the successful init.
            unsafe { CoUninitialize() };
        }
    }
}
