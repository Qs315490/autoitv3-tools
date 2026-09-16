//! Answer the resource chain from files, in front of the native Windows
//! layer.
//!
//! See [`super`] for what the chain is and why files can stand in for an
//! image; this module is the [`Platform`] layer that does it.

use autoitv3_runtime::host::HostContext;
use autoitv3_runtime::platform::Platform;
use autoitv3_runtime::value::Value;
use autoitv3_runtime::RuntimeError;

use super::ResourceFiles;
use crate::winfmt::Selector;

/// The module handle [`FileResourceLayer`] reports for the script's own image.
///
/// Not a real Win32 handle — nothing is mapped — so the value is chosen to be
/// recognisable and to collide with nothing: only calls carrying it are
/// answered by the layer.
pub(crate) const FILE_MODULE: usize = 0x00A3_0001;
/// The first `HRSRC` the layer hands out.
pub(crate) const FILE_HRSRC_BASE: usize = 0x00A3_1000;
/// The first `HGLOBAL` the layer hands out, one per locked resource.
pub(crate) const FILE_HGLOBAL_BASE: usize = 0x00A3_2000;

/// Answer a script's resource chain from files, with real memory.
///
/// On Windows the chain is real Win32, and the host process is `au3`, whose
/// image carries none of the script's resources: without an image to map, the
/// real calls find nothing. This layer sits **in front of** the native one and
/// answers the five calls of the chain itself:
///
/// | call | answer |
/// |---|---|
/// | `GetModuleHandleW(NULL)` | [`FILE_MODULE`], standing in for the script's own image |
/// | `FindResourceW` | the bytes of the named file, reached through the script's table or the staging names |
/// | `SizeofResource` / `LoadResource` | handles onto those bytes |
/// | `LockResource` | a **real pointer** into memory this layer owns |
///
/// A real pointer is the point: the script copies the payload out with a native
/// `RtlMoveMemory`, so an emulated address would not survive the trip. Anything
/// that does not carry one of the layer's sentinels is left to the native
/// layer, so a resource in some other module still goes through real Win32.
pub(crate) struct FileResourceLayer {
    files: ResourceFiles,
    /// The bytes the chain has handed out. Nothing is ever removed: a pointer a
    /// script locked stays valid for as long as the script can read it, and the
    /// buffers do not move when this vector grows (only the `Vec<u8>` headers
    /// do).
    blobs: Vec<Vec<u8>>,
    /// `(hrsrc, blob)` for each `FindResourceW` this layer answered.
    handles: Vec<(usize, usize)>,
}

impl FileResourceLayer {
    /// The layer, or `None` when there is nothing for it to answer from.
    pub(crate) fn new(files: ResourceFiles) -> Option<Self> {
        (!files.is_empty()).then(|| Self {
            files,
            blobs: Vec::new(),
            handles: Vec::new(),
        })
    }

    /// Whether a `DllCall` argument is the module handle this layer reports.
    fn is_file_module(value: Option<&Value>) -> bool {
        matches!(value, Some(Value::Int(handle)) if *handle as usize == FILE_MODULE)
    }

    /// The blob behind one of this layer's `HRSRC` handles.
    fn blob_of(&self, value: Option<&Value>) -> Option<usize> {
        let Value::Int(handle) = value? else {
            return None;
        };
        let handle = *handle as usize;
        self.handles
            .iter()
            .find(|(recorded, _)| *recorded == handle)
            .map(|(_, blob)| *blob)
    }

    /// `FindResourceW(hMod, name, type)`: ours only.
    fn find_resource(&mut self, pairs: &[(String, Value)]) -> Option<Value> {
        if !Self::is_file_module(pairs.first().map(|(_, value)| value)) {
            return None;
        }
        let Some(name) = pairs.get(1).and_then(|(_, value)| selector(value)) else {
            // Our module, but nothing to look a resource up by.
            return Some(Value::Int(0));
        };
        let Some(bytes) = self.files.find(&name) else {
            // A missing resource is a null handle, as Win32 answers it.
            return Some(Value::Int(0));
        };
        self.blobs.push(bytes);
        let handle = FILE_HRSRC_BASE + self.handles.len();
        self.handles.push((handle, self.blobs.len() - 1));
        Some(Value::Int(handle as i64))
    }

    /// `SizeofResource(hMod, hResInfo)`.
    fn size_of_resource(&self, pairs: &[(String, Value)]) -> Option<Value> {
        if !Self::is_file_module(pairs.first().map(|(_, value)| value)) {
            return None;
        }
        let size = self
            .blob_of(pairs.get(1).map(|(_, value)| value))
            .map(|blob| self.blobs[blob].len())
            .unwrap_or(0);
        Some(Value::Int(size as i64))
    }

    /// `LoadResource(hMod, hResInfo)`.
    fn load_resource(&self, pairs: &[(String, Value)]) -> Option<Value> {
        if !Self::is_file_module(pairs.first().map(|(_, value)| value)) {
            return None;
        }
        let Some(blob) = self.blob_of(pairs.get(1).map(|(_, value)| value)) else {
            return Some(Value::Int(0));
        };
        Some(Value::Int((FILE_HGLOBAL_BASE + blob) as i64))
    }

    /// `LockResource(hGlobal)`: a real address into this layer's own bytes.
    fn lock_resource(&self, pairs: &[(String, Value)]) -> Option<Value> {
        let Some(Value::Int(handle)) = pairs.first().map(|(_, value)| value) else {
            return None;
        };
        let address = (*handle as usize)
            .checked_sub(FILE_HGLOBAL_BASE)
            .filter(|blob| *blob < self.blobs.len())
            .map(|blob| self.blobs[blob].as_ptr())?;
        Some(Value::Int(address as i64))
    }
}

impl Platform for FileResourceLayer {
    fn name(&self) -> &'static str {
        "file-resources"
    }

    /// `DllCall` only — and it must say so: [`CompositePlatform`] skips a layer
    /// whose `provides` is false, so without this the layer would never be
    /// asked and the chain would go to real Win32 after all.
    ///
    /// [`CompositePlatform`]: crate::CompositePlatform
    fn provides(&self, name: &str) -> bool {
        name.eq_ignore_ascii_case("dllcall")
    }

    fn call(
        &mut self,
        name: &str,
        args: Vec<Value>,
        ctx: &mut dyn HostContext,
    ) -> Result<Option<Value>, RuntimeError> {
        if !name.eq_ignore_ascii_case("dllcall") {
            return Ok(None);
        }
        // `DllCall(dll, rettype, function, type, value, ...)`, the same shape
        // the emulation reads.
        let function = args.get(2).map(|value| value.to_autoit_string()).unwrap_or_default();
        let pairs: Vec<(String, Value)> = args
            .get(3..)
            .unwrap_or(&[])
            .chunks(2)
            .filter(|pair| pair.len() == 2)
            .map(|pair| (pair[0].to_autoit_string(), pair[1].clone()))
            .collect();

        let answered = match function.trim().to_ascii_lowercase().as_str() {
            "getmodulehandlew" | "getmodulehandlea" | "getmodulehandle" => {
                // Only the `NULL` form, which names "the module this script runs
                // from" — the one this layer is standing in for. A named module
                // is a real load and belongs to the native layer. (The check is
                // on the *type*: AutoIt coerces a name to 0 numerically, so
                // testing the value would capture `GetModuleHandleW("x.dll")`.)
                let null = match pairs.first().map(|(_, value)| value) {
                    Some(Value::Int(handle)) => *handle == 0,
                    Some(_) => false,
                    None => true,
                };
                null.then(|| Value::Int(FILE_MODULE as i64))
            }
            "findresourcew" | "findresourcea" | "findresourceexw" | "findresourceexa" => {
                self.find_resource(&pairs)
            }
            "sizeofresource" => self.size_of_resource(&pairs),
            "loadresource" => self.load_resource(&pairs),
            "lockresource" => self.lock_resource(&pairs),
            _ => None,
        };
        let Some(retval) = answered else {
            return Ok(None);
        };

        // `[return value, arg1, arg2, ...]`, the shape every DllCall returns.
        let mut result = Vec::with_capacity(pairs.len() + 1);
        result.push(retval);
        result.extend(pairs.iter().map(|(_, value)| value.clone()));
        ctx.set_error(0, 0);
        Ok(Some(Value::array(result)))
    }
}

/// A `FindResourceW` selector from one argument: a string name, an integer id,
/// or `None` for anything else.
fn selector(value: &Value) -> Option<Selector> {
    match value {
        Value::Str(name) => Some(Selector::name(name.clone())),
        other if other.is_number() => {
            let id = other.to_int();
            (id != 0).then(|| Selector::id(id as u32))
        }
        _ => None,
    }
}
