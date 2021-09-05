#![doc(html_root_url = "https://docs.rs/wasm_plugin_host/0.1.7")]
#![deny(missing_docs)]

//! A low-ish level tool for easily hosting WASM based plugins.
//!
//! The goal of wasm_plugin is to make communicating across the host-plugin
//! boundary as simple and idiomatic as possible while being unopinionated
//!  about how you actually use the plugin.
//!
//! Plugins should be written using [wasm_plugin_guest](https://crates.io/crates/wasm_plugin_guest)
//!
//! Loading a plugin is as simple as reading the .wasm file off disk.
//!
//! ```rust
//! # use std::error::Error;
//! #
//! # fn main() -> Result<(), Box<dyn Error>> {
//! let mut plugin = WasmPluginBuilder::from_file("path/to/plugin.wasm")?.finish()?;
//! #
//! #     Ok(())
//! # }
//! ```
//!
//! Calling functions exported by the plugin takes one of two forms. Either
//!  the function takes no arguments and returns a single serde deserializable
//! value:
//!
//! ```rust
//! # #[derive(Deserialize)]
//! # struct ResultType;
//! # use std::error::Error;
//! #
//! # fn main() -> Result<(), Box<dyn Error>> {
//! # let mut plugin = WasmPluginBuilder::from_file("path/to/plugin.wasm")?.finish()?;
//! let response: ResultType = plugin.call_function("function_name")?;
//! #
//! #     Ok(())
//! # }
//! ```
//! Or it takes a single serializable argument and returns a single result:
//! ```rust
//! # #[derive(Deserialize)]
//! # struct ResultType;
//! # #[derive(Serialize, Default)]
//! # struct Message;
//! # use std::error::Error;
//! #
//! # fn main() -> Result<(), Box<dyn Error>> {
//! # let mut plugin = WasmPluginBuilder::from_file("path/to/plugin.wasm")?.finish()?;
//! let message = Message::default();
//! let response: ResultType = plugin.call_function_with_argument("function_name", &message)?;
//! #
//! #     Ok(())
//! # }
//! ```
//! If the `inject_getrandom` feature is selected then the host's getrandom
//! will be injected into the plugin which allows `rand` to be used in the
//! plugin. `inject_getrandom` is selected by default.
//!
//! Currently serialization uses either bincode or json, selected by feature:
//! `serialize_bincode`: Uses serde and bincode. It is selected by default.
//! `serialize_json`: Uses serde and serde_json.
//! `serialize_nanoserde_json': Uses nanoserde.
//!
//! Bincode is likely the best choice if all plugins the system uses will be
//! written in Rust. Json is useful if a mix of languages will be used.
//!
//! ## Limitations
//!
//! There is no reflection so you must know up front which functions
//! a plugin exports and their signatures.

use std::{
    path::Path,
    sync::{Arc, Mutex},
};

use wasmtime::{Func, Instance, Memory, Module, Store, Engine, Linker, Caller, TypedFunc, AsContextMut, AsContext};
pub use wasmtime::{Extern};

#[allow(missing_docs)]
pub mod errors;
#[allow(missing_docs)]
pub mod serialization;
use bitfield::bitfield;
use serialization::{Deserializable, Serializable};
use crate::errors::WasmPluginError;

bitfield! {
    #[doc(hidden)]
    pub struct FatPointer(u64);
    impl Debug;
    u32;
    ptr, set_ptr: 31, 0;
    len, set_len: 63, 32;
}

#[derive(Clone)]
struct Env<C>
where
    C: Send + Sync + Clone + 'static,
{
    allocator: Option<TypedFunc<u32, u32>>,
    memory: Option<Memory>,
    garbage: Arc<Mutex<Vec<FatPointer>>>,
    ctx: C,
}

impl<C: Send + Sync + Clone + 'static> Env<C> {
    fn new(garbage: Arc<Mutex<Vec<FatPointer>>>, ctx: C) -> Self {
        Self {
            allocator: None,
            memory: None,
            garbage,
            ctx,
        }
    }

    // A new message buffer from this Env
    fn message_buffer(&self) -> MessageBuffer {
        MessageBuffer {
            allocator: self.allocator.unwrap(), // FIXME: these were get_unchecked
            memory: self.memory.unwrap(),
            garbage: vec![],
        }
    }
}

/// Constructs a WasmPlugin
pub struct WasmPluginBuilder {
    module: Module,
    store: Store<Env<()>>,
    env: Linker<Env<()>>,
    // TODO: Can we do this without the lock?
    garbage: Arc<Mutex<Vec<FatPointer>>>,
}
impl WasmPluginBuilder {
    /// Load a plugin off disk and prepare it for use.
    pub fn from_file(path: impl AsRef<Path>) -> errors::Result<Self> {
        let source = std::fs::read(path)?;
        Self::from_source(&source)
    }

    /// Load a plugin from WASM source and prepare it for use.
    pub fn from_source(source: &[u8]) -> errors::Result<Self> {
        let engine = Engine::default();
        let mut linker = Linker::new(&engine);
        linker.allow_shadowing(true); // FIXME: Keep this?
        let module = Module::new(&engine, source)
            .map_err(WasmPluginError::WasmerCompileError)?;
        let garbage: Arc<Mutex<Vec<FatPointer>>> = Default::default();
        let store = Store::new(&engine, Env::new(Arc::clone(&garbage), ()));
        linker.func_wrap("env", "abort", |_: u32, _: u32, _: i32, _: i32| {}).unwrap(); // FIXME note; "env"

        #[cfg(feature = "inject_getrandom")]
        {
            // TODO: FIXME
            // env.insert(
            //     "__getrandom",
            //     Function::new_native_with_env(
            //         &store,
            //         Env::new(garbage.clone(), ()),
            //         getrandom_shim,
            //     ),
            // );
        }

        Ok(Self {
            module,
            store,
            env: linker,
            garbage,
        })
    }

    fn import(mut self, name: impl ToString, value: impl Into<Extern>) -> Self {
        let name = format!("wasm_plugin_imported__{}", name.to_string()); // TODO: Use module name instead of adding prefix
        self.env.define("env", &name, value).unwrap(); // FIXME note unwrap, "env"
        self
    }

    // FIXME: There is a lot of problematic duplication in this code. I need
    // to sit down and come up with a better abstraction.

    /// Import a function defined in the host into the guest. The function's
    /// arguments and return type must all be serializable.
    /// An immutable reference to `ctx` will be passed to the function as it's
    /// first argument each time it's called.
    ///
    /// NOTE: This method exists due to a limitation in the underlying Waswer
    /// engine which currently doesn't support imported closures with
    /// captured context. The Wasamer developers have said they are interested
    /// in removing that limitation and when they do this method will be
    /// removed in favor of `import_function' since context can be more
    /// idiomatically handled with captured values.
    // pub fn import_function_with_context<
    //     Args,
    //     F: ImportableFnWithContext<C, Args> + Send + 'static,
    //     C: Send + Sync + Clone + 'static,
    // >(
    //     mut self,
    //     name: impl ToString,
    //     ctx: C,
    //     value: F,
    // ) -> Self {
    //     if F::has_arg() {
    //         let f = if F::has_return() {
    //             let wrapped = move |env: Caller<'_, Env<C>>, ptr: u32, len: u32| -> u64 {
    //                 let mut buffer = env.data().message_buffer();
    //                 let r = value
    //                     .call_with_input(&mut buffer, ptr as usize, len as usize, &ctx)
    //                     .unwrap()
    //                     .map(|p| p.0)
    //                     .unwrap_or(0);
    //                 env.data().garbage.lock().unwrap().extend(buffer.garbage.drain(..));
    //                 r
    //             };
    //             Func::wrap(&mut self.store, wrapped)
    //         } else {
    //             let wrapped = move |env: Caller<'_, Env<C>>, ptr: u32, len: u32| {
    //                 let mut buffer = env.data().message_buffer();
    //                 value
    //                     .call_with_input(&mut buffer, ptr as usize, len as usize, &ctx)
    //                     .unwrap();
    //                 env.data().garbage.lock().unwrap().extend(buffer.garbage.drain(..));
    //             };
    //             Func::wrap(&mut self.store, wrapped)
    //         };
    //         self.import(name, f)
    //     } else {
    //         let f = if F::has_return() {
    //             let wrapped = move |env: Caller<'_, Env<C>>| -> u64 {
    //                 let mut buffer = env.data().message_buffer();
    //                 let r = value
    //                     .call_without_input(&mut buffer, &ctx)
    //                     .unwrap()
    //                     .map(|p| p.0)
    //                     .unwrap_or(0);
    //                 env.data().garbage.lock().unwrap().extend(buffer.garbage.drain(..));
    //                 r
    //             };
    //             Func::wrap(&mut self.store, wrapped)
    //         } else {
    //             let wrapped = move |env: Caller<'_, Env<C>>| {
    //                 let mut buffer = env.data().message_buffer();
    //                 value.call_without_input(&mut buffer, &ctx).unwrap();
    //                 env.data().garbage.lock().unwrap().extend(buffer.garbage.drain(..));
    //             };
    //             Func::wrap(&mut self.store, wrapped)
    //         };
    //         self.import(name, f)
    //     }
    // }

    /// Import a function defined in the host into the guest. The function's
    /// arguments and return type must all be serializable.
    pub fn import_function<Args, F: ImportableFn<Args> + Send + Sync + 'static>(
        mut self,
        name: impl ToString,
        value: F,
    ) -> Self {
        if F::has_arg() {
            let f = if F::has_return() {
                let wrapped = move |mut env: Caller<'_, Env<()>>, ptr: u32, len: u32| -> u64 {
                    let mut buffer = env.data().message_buffer();
                    let r = value
                        .call_with_input(&mut env, &mut buffer, ptr as usize, len as usize)
                        .unwrap()
                        .map(|p| p.0)
                        .unwrap_or(0);
                    env.data().garbage.lock().unwrap().extend(buffer.garbage.drain(..));
                    r
                };
            Func::wrap(&mut self.store, wrapped)
            } else {
                let wrapped = move |mut env: Caller<'_, Env<()>>, ptr: u32, len: u32| {
                    let mut buffer = env.data().message_buffer();
                    value
                        .call_with_input(&mut env, &mut buffer, ptr as usize, len as usize)
                        .unwrap();
                    env.data().garbage.lock().unwrap().extend(buffer.garbage.drain(..));
                };
                Func::wrap(&mut self.store, wrapped)
            };
            self.import(name, f)
        } else {
            let f = if F::has_return() {
                let wrapped = move |mut env: Caller<'_, Env<()>>| -> u64 {
                    let mut buffer = env.data().message_buffer();
                    let r = value
                        .call_without_input(&mut env, &mut buffer)
                        .unwrap()
                        .map(|p| p.0)
                        .unwrap_or(0);
                    env.data().garbage.lock().unwrap().extend(buffer.garbage.drain(..));
                    r
                };
                Func::wrap(&mut self.store, wrapped)
            } else {
                let wrapped = move |mut env: Caller<'_, Env<()>>| {
                    let mut buffer = env.data().message_buffer();
                    value.call_without_input(&mut env, &mut buffer).unwrap();
                    env.data().garbage.lock().unwrap().extend(buffer.garbage.drain(..));
                };
                Func::wrap(&mut self.store, wrapped)
            };
            self.import(name, f)
        }
    }

    /// Finalize the builder and create the WasmPlugin ready for use.
    pub fn finish(mut self) -> errors::Result<WasmPlugin> {
        let instance = self.env.instantiate(&mut self.store, &self.module)
            .map_err(WasmPluginError::WasmerInstantiationError)?;
        let allocator = instance
            .get_typed_func::<u32, u32, _>(&mut self.store, "allocate_message_buffer")
            .map_err(WasmPluginError::WasmerRuntimeError)?;
        self.store.data_mut().allocator = Some(allocator);
        self.store.data_mut().memory = Some(instance.get_memory(&mut self.store, "memory").expect("FIXME"));
        Ok(WasmPlugin {
            store: self.store,
            instance,
            garbage: self.garbage,
        })
    }
}

/// A marker trait for Fn types who's arguments and return type can be
/// serialized and are thus safe to import into a plugin;
// pub trait ImportableFnWithContext<C, Arglist> {
//     #[doc(hidden)]
//     fn has_arg() -> bool;
//     #[doc(hidden)]
//     fn has_return() -> bool;
//     #[doc(hidden)]
//     fn call_with_input(
//         &self,
//         message_buffer: &mut MessageBuffer,
//         ptr: usize,
//         len: usize,
//         ctx: &C,
//     ) -> errors::Result<Option<FatPointer>>;
//     #[doc(hidden)]
//     fn call_without_input(
//         &self,
//         message_buffer: &mut MessageBuffer,
//         ctx: &C,
//     ) -> errors::Result<Option<FatPointer>>;
// }
//
// impl<C, Args, ReturnType, F> ImportableFnWithContext<C, Args> for F
// where
//     F: Fn(&C, Args) -> ReturnType,
//     Args: Deserializable,
//     ReturnType: Serializable,
// {
//     fn has_arg() -> bool {
//         true
//     }
//     fn has_return() -> bool {
//         std::mem::size_of::<ReturnType>() > 0
//     }
//     fn call_with_input(
//         &self,
//         message_buffer: &mut MessageBuffer,
//         ptr: usize,
//         len: usize,
//         ctx: &C,
//     ) -> errors::Result<Option<FatPointer>> {
//         let message = message_buffer.read_message(ptr, len);
//         let result = self(ctx, Args::deserialize(&message)?);
//         if std::mem::size_of::<ReturnType>() > 0 {
//             // No need to write anything for ZSTs
//             let message = result.serialize()?;
//             Ok(Some(message_buffer.write_message(&message)))
//         } else {
//             Ok(None)
//         }
//     }
//
//     fn call_without_input(
//         &self,
//         _message_buffer: &mut MessageBuffer,
//         _ctx: &C,
//     ) -> errors::Result<Option<FatPointer>> {
//         unimplemented!("Requires argument")
//     }
// }
//
// impl<C, ReturnType, F> ImportableFnWithContext<C, NoArgs> for F
// where
//     F: Fn(&C) -> ReturnType,
//     ReturnType: Serializable,
// {
//     fn has_arg() -> bool {
//         false
//     }
//     fn has_return() -> bool {
//         std::mem::size_of::<ReturnType>() > 0
//     }
//     fn call_with_input(
//         &self,
//         _message_buffer: &mut MessageBuffer,
//         _ptr: usize,
//         _len: usize,
//         _ctx: &C,
//     ) -> errors::Result<Option<FatPointer>> {
//         unimplemented!("Must not supply argument")
//     }
//
//     fn call_without_input(
//         &self,
//         message_buffer: &mut MessageBuffer,
//         ctx: &C,
//     ) -> errors::Result<Option<FatPointer>> {
//         let result = self(ctx);
//         if std::mem::size_of::<ReturnType>() > 0 {
//             // No need to write anything for ZSTs
//             let message = result.serialize()?;
//             Ok(Some(message_buffer.write_message(&message)))
//         } else {
//             Ok(None)
//         }
//     }
// }

/// A marker trait for Fn types who's arguments and return type can be
/// serialized and are thus safe to import into a plugin;
pub trait ImportableFn<ArgList> {
    #[doc(hidden)]
    fn has_arg() -> bool;
    #[doc(hidden)]
    fn has_return() -> bool;
    #[doc(hidden)]
    fn call_with_input<S: AsContextMut>(
        &self,
        store: S,
        message_buffer: &mut MessageBuffer,
        ptr: usize,
        len: usize,
    ) -> errors::Result<Option<FatPointer>>;
    #[doc(hidden)]
    fn call_without_input<S: AsContextMut>(
        &self,
        store: S,
        message_buffer: &mut MessageBuffer,
    ) -> errors::Result<Option<FatPointer>>;
}

impl<F, Args, ReturnType> ImportableFn<Args> for F
where
    F: Fn(Args) -> ReturnType,
    Args: Deserializable,
    ReturnType: Serializable,
{
    fn has_arg() -> bool {
        true
    }
    fn has_return() -> bool {
        std::mem::size_of::<ReturnType>() > 0
    }
    fn call_with_input<S: AsContextMut>(
        &self,
        store: S,
        message_buffer: &mut MessageBuffer,
        ptr: usize,
        len: usize,
    ) -> errors::Result<Option<FatPointer>> {
        // MessageBuffer {
        //     allocator: self.allocator.as_ref().unwrap(), // FIXME: these were get_unchecked
        //     memory: self.memory.as_ref().unwrap(),
        //     garbage: vec![],
        // }

        let message = message_buffer.read_message(ptr, len, &store);
        let result = self(Args::deserialize(&message)?);
        if std::mem::size_of::<ReturnType>() > 0 {
            let message = result.serialize()?;
            Ok(Some(message_buffer.write_message(&message, store)))
        } else {
            // No need to write anything for ZSTs
            Ok(None)
        }
    }

    fn call_without_input<S: AsContextMut>(
        &self,
        _store: S,
        _message_buffer: &mut MessageBuffer,
    ) -> errors::Result<Option<FatPointer>> {
        unimplemented!("Requires argument")
    }
}

#[doc(hidden)]
pub enum NoArgs {}

impl<F, ReturnType> ImportableFn<NoArgs> for F
where
    F: Fn() -> ReturnType,
    ReturnType: Serializable,
{
    fn has_arg() -> bool {
        false
    }
    fn has_return() -> bool {
        std::mem::size_of::<ReturnType>() > 0
    }
    fn call_with_input<S: AsContextMut>(
        &self,
        _store: S,
        _message_buffer: &mut MessageBuffer,
        _ptr: usize,
        _len: usize,
    ) -> errors::Result<Option<FatPointer>> {
        unimplemented!("Must not supply argument")
    }

    fn call_without_input<S: AsContextMut>(
        &self,
        store: S,
        message_buffer: &mut MessageBuffer,
    ) -> errors::Result<Option<FatPointer>> {
        let result = self();
        if std::mem::size_of::<ReturnType>() > 0 {
            // No need to write anything for ZSTs
            let message = result.serialize()?;
            Ok(Some(message_buffer.write_message(&message, store)))
        } else {
            Ok(None)
        }
    }
}

/// A loaded plugin
pub struct WasmPlugin {
    store: Store<Env<()>>,
    instance: Instance,
    garbage: Arc<Mutex<Vec<FatPointer>>>,
}

#[doc(hidden)]
pub struct MessageBuffer {
    memory: Memory,
    allocator: TypedFunc<u32, u32>,
    garbage: Vec<FatPointer>,
}

impl MessageBuffer {
    fn write_message<S: AsContextMut>(&mut self, message: &[u8], mut store: S) -> FatPointer {
        let len = message.len() as u32;

        // let ptr = self
        //     .allocator
        //     .native::<u32, u32>()
        //     .unwrap()
        //     .call(len as u32)
        //     .unwrap();
        let ptr = self.allocator.call(&mut store, len).unwrap(); // FIXME: unwrap?

        unsafe {
            let data = self.memory.data_mut(&mut store); // FIXME: use memory.write instead of unsafe
            data[ptr as usize..ptr as usize + len as usize].copy_from_slice(&message); // Might panic if memory isn't large enough. Is this an issue?
        }

        let mut fat_ptr = FatPointer(0);
        fat_ptr.set_ptr(ptr);
        fat_ptr.set_len(len);
        self.garbage.push(FatPointer(fat_ptr.0));
        fat_ptr
    }

    fn read_message<S: AsContext>(&self, ptr: usize, len: usize, store: S) -> Vec<u8> {
        let mut buff: Vec<u8> = vec![0; len];
        self.memory.read(&store, ptr, &mut buff).expect("FIXME");
        buff
    }

    fn read_message_from_fat_pointer<S: AsContext>(&self, fat_ptr: u64, store: S) -> Vec<u8> {
        let fat_ptr = FatPointer(fat_ptr);
        let mut buff: Vec<u8> = vec![0; fat_ptr.len() as usize];
        self.memory.read(&store, fat_ptr.ptr() as usize, &mut buff).expect("FIXME");
        buff
    }
}

impl WasmPlugin {
    fn message_buffer(&mut self) -> errors::Result<MessageBuffer> {
        let allocator = self.instance
            .get_typed_func::<u32, u32, _>(&mut self.store, "allocate_message_buffer")
            .map_err(WasmPluginError::WasmerRuntimeError)?;
        Ok(MessageBuffer {
            memory: self.instance.get_memory(&mut self.store, "memory").expect("FIXME"),
            allocator,
            garbage: vec![],
        })
    }

    /// Call a function exported by the plugin with a single argument
    /// which will be serialized and sent to the plugin.
    ///
    /// Deserialization of the return value depends on the type being known
    /// at the call site.
    pub fn call_function_with_argument<ReturnType, Args>(
        &mut self,
        fn_name: &str,
        args: &Args,
    ) -> errors::Result<ReturnType>
    where
        Args: Serializable,
        ReturnType: Deserializable,
    {
        let message = args.serialize()?;
        let mut buffer = self.message_buffer()?;
        let ptr = buffer.write_message(&message, &mut self.store);

        let buff = self.call_function_raw(fn_name, Some(ptr))?;
        drop(buffer);
        ReturnType::deserialize(&buff)
    }

    fn call_function_raw(
        &mut self,
        fn_name: &str,
        input_buffer: Option<FatPointer>,
    ) -> errors::Result<Vec<u8>> {
        let f = self
            .instance
            // .exports(&mut self.store)
            .get_func(&mut self.store, &format!("wasm_plugin_exported__{}", fn_name))
            .unwrap_or_else(|| panic!("Unable to find function {}", fn_name)); // FIXME

        let ptr = if let Some(fat_ptr) = input_buffer {
            f.typed::<(u32, u32), u64, _>(&self.store).expect("FIXME: anyhow::Error")
                .call(&mut self.store, (fat_ptr.ptr() as u32, fat_ptr.len() as u32)).expect("FIXME: trap")
        } else {
            f.typed::<(), u64, _>(&self.store).expect("FIXME: anyhow::Error").call(&mut self.store, ()).expect("FIXME: trap")
        };
        let result = self.message_buffer()?.read_message_from_fat_pointer(ptr, &self.store);

        let mut garbage: Vec<_> = self.garbage.lock().unwrap().drain(..).collect();

        if FatPointer(ptr).len() > 0 {
            garbage.push(FatPointer(ptr));
        }
        if !garbage.is_empty() {
            let f = self
                .instance
                // .exports(&mut self.store)
                .get_typed_func::<(u32, u32), (), _>(&mut self.store, "free_message_buffer")
                .unwrap_or_else(|_| panic!("Unable to find function 'free_message_buffer'")); // FIXME
            for fat_ptr in garbage {
                f.call(&mut self.store, (fat_ptr.ptr() as u32, fat_ptr.len() as u32), ).expect("FIXME: trap")
            }
        }

        Ok(result)
    }

    /// Call a function exported by the plugin.
    ///
    /// Deserialization of the return value depends on the type being known
    /// at the call site.
    pub fn call_function<ReturnType>(&mut self, fn_name: &str) -> errors::Result<ReturnType>
    where
        ReturnType: Deserializable,
    {
        let buff = self.call_function_raw(fn_name, None)?;
        ReturnType::deserialize(&buff)
    }
}

#[cfg(feature = "inject_getrandom")]
fn getrandom_shim(mut env: Caller<'_, ()>, ptr: u32, len: u32) {
    // FIXME
    // if let Some(Extern::Memory(memory)) = env.get_export("memory") {
    //     let view: MemoryView<u8> = memory.view();
    //     let mut buff: Vec<u8> = vec![0; len as usize];
    //     getrandom::getrandom(&mut buff).unwrap();
    //     for (dst, src) in view[ptr as usize..ptr as usize + len as usize]
    //         .iter()
    //         .zip(buff)
    //     {
    //         dst.set(src);
    //     }
    // }
}
