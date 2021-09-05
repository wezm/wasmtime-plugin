/// Error returned by WasmPlugin when loading plugins or calling functions.
pub enum WasmPluginError {
    /// A problem compiling the plugin's WASM source
    WasmerCompileError(anyhow::Error),
    /// A problem instantiating the Wasmer runtime
    WasmerInstantiationError(anyhow::Error),
    /// A problem interacting with the plugin
    WasmerRuntimeError(anyhow::Error),
    /// A problem getting an export from the plugin
    WasmerExportError(anyhow::Error),
    /// A problem loading the plugin's source from disk
    IoError(std::io::Error),
    /// A problems serializing an argument to send to one of the plugin's
    /// functions.
    SerializationError,
    /// A problem deserializing the return value of a call to one of the
    /// plugin's functions. This almost always represents a type mismatch
    /// between the callsite in the host and the function signature in the
    /// plugin.
    DeserializationError,
    /// A problem decoding the utf8 sent by the plugin
    #[cfg(feature = "serialize_nanoserde_json")]
    FromUtf8Error(std::string::FromUtf8Error),
}

impl std::error::Error for WasmPluginError {}

impl core::fmt::Debug for WasmPluginError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        core::fmt::Display::fmt(self, f)
    }
}

impl core::fmt::Display for WasmPluginError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            WasmPluginError::WasmerCompileError(e) => e.fmt(f),
            WasmPluginError::WasmerInstantiationError(e) => e.fmt(f),
            WasmPluginError::WasmerRuntimeError(e) => e.fmt(f),
            WasmPluginError::WasmerExportError(e) => e.fmt(f),
            WasmPluginError::IoError(e) => e.fmt(f),

            WasmPluginError::SerializationError => write!(f, "There was a problem serializing the argument to the function call"),
            WasmPluginError::DeserializationError=> write!(f, "There was a problem deserializing the value returned by the plugin function. This almost certainly means that the type at the call site does not match the type in the plugin's function signature."),
            #[cfg(feature = "serialize_nanoserde_json")]
            WasmPluginError::FromUtf8Error(e) => e.fmt(f),
        }
    }
}

impl From<std::io::Error> for WasmPluginError {
    fn from(e: std::io::Error) -> WasmPluginError {
        WasmPluginError::IoError(e)
    }
}

#[cfg(feature = "serialize_nanoserde_json")]
impl From<std::string::FromUtf8Error> for WasmPluginError {
    fn from(e: std::string::FromUtf8Error) -> WasmPluginError {
        WasmPluginError::FromUtf8Error(e)
    }
}

pub type Result<T> = std::result::Result<T, WasmPluginError>;
