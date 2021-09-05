use std::{env, process};

use wasmtime_plugin_host::WasmPluginBuilder;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let plugin_path = env::args().skip(1).next();
    if plugin_path.is_none() {
        eprintln!("Usage: example_host path/to/plugin.wasm");
        process::exit(1);
    }
    let mut plugin = WasmPluginBuilder::from_file(plugin_path.unwrap())?
    .import_function("the_hosts_favorite_numbers", || {
        vec![0, 1, 42]
    })
    .import_function("please_capitalize_this", |mut s: String| {
        println!("From guest: {}", s);
        s.make_ascii_uppercase();
        s
    })
    .finish()?;
    let response: String = plugin.call_function("hello")?;
    println!("The guest says: '{}'", response);

    let message = "Hello, Guest!".to_string();
    let response: String = plugin.call_function_with_argument("echo", &message)?;
    println!(
        "I said: '{}'. The guest said, '{}' back. Weird",
        message, response
    );

    // Any type that can be serialized works
    let response: Vec<i32> = plugin.call_function("favorite_numbers")?;
    println!(
        "The guest's favorite integers (less that 2**32) are: '{:?}'",
        response
    );

    Ok(())
}
