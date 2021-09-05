#[wasmtime_plugin_guest::export_function]
fn hello() -> String {
    "Hello, Host!".to_string()
}

#[wasmtime_plugin_guest::export_function]
fn echo(message: String) -> String {
    let message = please_capitalize_this(message);
    please_capitalize_this(message.clone());
    format!("{}", message)
}

wasmtime_plugin_guest::import_functions!{
    fn the_hosts_favorite_numbers() -> Vec<i32>;
    fn please_capitalize_this(s: String) -> String;
}

#[wasmtime_plugin_guest::export_function]
fn favorite_numbers() -> Vec<i32> {
    let numbers = the_hosts_favorite_numbers();
    numbers.into_iter().map(|n| n+1).collect()
}
