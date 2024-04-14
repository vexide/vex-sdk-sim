//! Example of instantiating two modules which link to each other.

// You can execute this example with `cargo run --example linking`

use std::ffi::CStr;
use std::fs::File;
use std::io::BufWriter;
use std::time::Instant;

use anyhow::Context;
use piet::{
    kurbo::{Circle, Rect},
    Color, FontFamily, ImageFormat, RenderContext, Text, TextAttribute, TextLayout,
    TextLayoutBuilder,
};
use piet_common::kurbo::{Point, Size};
use piet_common::{BitmapTarget, Device};
use png::{ColorType, Encoder};
use wasmtime::*;

use crate::sdk::{Sdk, SdkState};

mod sdk;

fn main() -> Result<()> {
    println!("Compiling...");
    let engine = Engine::new(
        Config::new()
            .debug_info(true)
            .wasm_backtrace_details(WasmBacktraceDetails::Enable),
    )?;
    let module = Module::from_file(
        &engine,
        "../vexide-wasm/target/wasm32-unknown-unknown/debug/examples/basic.wasm",
    )?;
    println!("Booting...");
    let mut renderer = Device::new().unwrap();
    let mut state = SdkState::new(&mut renderer);
    let mut store = Store::new(&engine, state);
    let imports = module
        .imports()
        .filter_map(|i| match i.ty() {
            ExternType::Table(table_ty) => Some(table_ty),
            _ => None,
        })
        .next()
        .unwrap();

    // First set up our linker which is going to be linking modules together. We
    // want our linker to have wasi available, so we set that up here as well.
    let mut linker = Linker::new(&engine);
    let table = Table::new(&mut store, imports, Ref::Func(None))?;
    linker.define(&store, "env", "__indirect_function_table", table)?;
    linker.func_wrap(
        "env",
        "sim_log_backtrace",
        |caller: Caller<'_, SdkState>| {
            let backtrace = WasmBacktrace::capture(caller);
            println!("backtrace: {:?}", backtrace);
            Ok(())
        },
    )?;

    // Load and compile our module

    let instance = linker.instantiate(&mut store, &module)?;

    let memory = instance.get_memory(&mut store, "memory").unwrap();

    let target_pages = 0x700;
    let memory_size = memory.size(&store);
    memory.grow(&mut store, target_pages - memory_size)?;

    let sdk = Sdk::new(&mut store, memory);
    sdk.expose_jump_table(&mut store, &table, &memory)?;

    println!("_entry()");
    let run = instance.get_typed_func::<(), ()>(&mut store, "_entry")?;
    run.call(&mut store, ())?;
    Ok(())
}
