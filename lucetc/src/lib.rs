mod bindings;
mod compiler;
mod decls;
mod error;
mod function;
mod heap;
mod load;
mod module;
mod name;
mod output;
mod patch;
mod pointer;
mod runtime;
mod sparsedata;
mod stack_probe;
mod table;
mod traps;

pub use crate::{
    bindings::Bindings, compiler::Compiler, compiler::OptLevel, error::LucetcError,
    heap::HeapSettings, load::read_module, patch::patch_module,
};
use failure::{format_err, Error, ResultExt};
use parity_wasm::elements::serialize;
use parity_wasm::elements::Module;
use std::path::Path;
use tempfile;

pub struct Lucetc {
    module: Module,
    bindings: Bindings,
    opt_level: OptLevel,
    heap: HeapSettings,
}

impl Lucetc {
    pub fn new<P: AsRef<Path>>(input: P) -> Result<Self, LucetcError> {
        let input = input.as_ref();
        let module = read_module(input)?;
        Ok(Self {
            module,
            bindings: Bindings::empty(),
            opt_level: OptLevel::default(),
            heap: HeapSettings::default(),
        })
    }

    pub fn bindings(mut self, bindings: Bindings) -> Result<Self, Error> {
        self.with_bindings(bindings)?;
        Ok(self)
    }
    pub fn with_bindings(&mut self, bindings: Bindings) -> Result<(), Error> {
        self.bindings.extend(bindings)
    }

    pub fn opt_level(mut self, opt_level: OptLevel) -> Self {
        self.with_opt_level(opt_level);
        self
    }
    pub fn with_opt_level(&mut self, opt_level: OptLevel) {
        self.opt_level = opt_level;
    }

    pub fn builtins<P: AsRef<Path>>(mut self, builtins: P) -> Result<Self, Error> {
        self.with_builtins(builtins)?;
        Ok(self)
    }
    pub fn with_builtins<P: AsRef<Path>>(&mut self, builtins_path: P) -> Result<(), Error> {
        let (newmodule, builtins_map) = patch_module(self.module.clone(), builtins_path)?;
        self.module = newmodule;
        self.bindings.extend(Bindings::env(builtins_map))?;
        Ok(())
    }

    pub fn min_reserved_size(mut self, min_reserved_size: u64) -> Self {
        self.with_min_reserved_size(min_reserved_size);
        self
    }
    pub fn with_min_reserved_size(&mut self, min_reserved_size: u64) {
        self.heap.min_reserved_size = min_reserved_size;
    }

    pub fn max_reserved_size(mut self, max_reserved_size: u64) -> Self {
        self.with_max_reserved_size(max_reserved_size);
        self
    }
    pub fn with_max_reserved_size(&mut self, max_reserved_size: u64) {
        self.heap.max_reserved_size = max_reserved_size;
    }

    pub fn guard_size(mut self, guard_size: u64) -> Self {
        self.with_guard_size(guard_size);
        self
    }
    pub fn with_guard_size(&mut self, guard_size: u64) {
        self.heap.guard_size = guard_size;
    }

    pub fn object_file<P: AsRef<Path>>(self, output: P) -> Result<(), Error> {
        let module_contents = serialize(self.module)?;

        let compiler = Compiler::new(&module_contents, self.opt_level, &self.bindings, self.heap)?;
        let obj = compiler.object_file()?;

        obj.write(output.as_ref()).context("writing object file")?;
        Ok(())
    }

    pub fn clif_ir<P: AsRef<Path>>(self, output: P) -> Result<(), Error> {
        let module_contents = serialize(self.module)?;

        let compiler = Compiler::new(&module_contents, self.opt_level, &self.bindings, self.heap)?;

        compiler
            .cranelift_funcs()?
            .write(&output)
            .context("writing clif file")?;

        Ok(())
    }

    pub fn shared_object_file<P: AsRef<Path>>(self, output: P) -> Result<(), Error> {
        let dir = tempfile::Builder::new().prefix("lucetc").tempdir()?;
        let objpath = dir.path().join("tmp.o");
        self.object_file(objpath.clone())?;
        link_so(objpath, output)?;
        Ok(())
    }
}

fn link_so<P, Q>(objpath: P, sopath: Q) -> Result<(), Error>
where
    P: AsRef<Path>,
    Q: AsRef<Path>,
{
    use std::process::Command;
    let mut cmd_ld = Command::new("ld");
    cmd_ld.arg(objpath.as_ref());
    cmd_ld.arg("-shared");
    cmd_ld.arg("-o");
    cmd_ld.arg(sopath.as_ref());

    let run_ld = cmd_ld
        .output()
        .context(format_err!("running ld on {:?}", objpath.as_ref()))?;

    if !run_ld.status.success() {
        Err(format_err!(
            "ld of {} failed: {}",
            objpath.as_ref().to_str().unwrap(),
            String::from_utf8_lossy(&run_ld.stderr)
        ))?;
    }
    Ok(())
}
