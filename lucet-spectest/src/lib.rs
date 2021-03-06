pub mod error;
pub mod instance;
pub mod script;

pub use crate::error::{SpecTestError, SpecTestErrorKind};
pub use crate::result::{command_description, SpecScriptResult};

mod bindings;
mod result;

use crate::instance::{ExportType, ValueType};
use crate::script::{ScriptEnv, ScriptError};
use failure::{format_err, Error, ResultExt};
use lucet_runtime::{Error as RuntimeError, TrapCodeType, UntypedRetVal, Val};
use std::fs;
use std::path::PathBuf;
use wabt::script::{Action, CommandKind, ScriptParser, Value};

pub fn run_spec_test(spec_path: &PathBuf) -> Result<SpecScriptResult, Error> {
    let wast = fs::read_to_string(spec_path)?;
    let mut parser: ScriptParser = ScriptParser::from_str(&wast)?;

    let mut script = ScriptEnv::new();
    let mut res = SpecScriptResult::new();

    while let Some(ref cmd) = parser.next()? {
        match step(&mut script, &cmd.kind) {
            Ok(()) => res.pass(cmd),
            Err(e) => match e.get_context() {
                SpecTestErrorKind::UnsupportedCommand | SpecTestErrorKind::UnsupportedLucetc => {
                    res.skip(cmd, e)
                }
                _ => res.fail(cmd, e),
            },
        }
    }

    Ok(res)
}

fn step(script: &mut ScriptEnv, cmd: &CommandKind) -> Result<(), SpecTestError> {
    match cmd {
        CommandKind::Module {
            ref module,
            ref name,
        } => {
            let module = module.clone().into_vec();
            script.instantiate(module, name).map_err(|e| {
                if e.unsupported() {
                    Error::from(e).context(SpecTestErrorKind::UnsupportedLucetc)
                } else {
                    Error::from(e).context(SpecTestErrorKind::UnexpectedFailure)
                }
            })?;
            Ok(())
        }

        CommandKind::AssertInvalid { ref module, .. } => {
            let module = module.clone().into_vec();
            match script.instantiate(module, &None) {
                Err(ScriptError::ValidationError(_)) => Ok(()),
                Ok(_) => {
                    script.delete_last();
                    Err(SpecTestErrorKind::UnexpectedSuccess)?
                }
                Err(e) => Err(Error::from(e).context(SpecTestErrorKind::UnexpectedFailure))?,
            }
        }

        CommandKind::AssertMalformed { ref module, .. } => {
            let module = module.clone().into_vec();
            match script.instantiate(module, &None) {
                Err(ScriptError::DeserializeError(_)) | Err(ScriptError::ValidationError(_)) => {
                    Ok(())
                }
                Ok(_) => Err(SpecTestErrorKind::UnexpectedSuccess)?,
                Err(e) => Err(Error::from(e).context(SpecTestErrorKind::UnexpectedFailure))?,
            }
        }

        CommandKind::AssertUninstantiable { module, .. } => {
            let module = module.clone().into_vec();
            match script.instantiate(module, &None) {
                Err(ScriptError::DeserializeError(_)) => Ok(()), // XXX This is probably the wrong ScriptError to look for - FIXME
                Ok(_) => Err(SpecTestErrorKind::UnexpectedSuccess)?,
                Err(e) => Err(Error::from(e).context(SpecTestErrorKind::UnexpectedFailure))?,
            }
        }

        CommandKind::AssertUnlinkable { module, .. } => {
            let module = module.clone().into_vec();
            match script.instantiate(module, &None) {
                Err(ScriptError::CompileError(_)) => Ok(()), // XXX could other errors trigger this too?
                Ok(_) => Err(SpecTestErrorKind::UnexpectedSuccess)?,
                Err(e) => Err(Error::from(e).context(SpecTestErrorKind::UnexpectedFailure))?,
            }
        }

        CommandKind::Register {
            ref name,
            ref as_name,
        } => {
            script
                .register(name, as_name)
                .context(SpecTestErrorKind::UnexpectedFailure)?;
            Ok(())
        }

        CommandKind::PerformAction(ref action) => match action {
            Action::Invoke {
                ref module,
                ref field,
                ref args,
            } => {
                let args = translate_args(args);
                let _res = script
                    .run(module, field, args)
                    .context(SpecTestErrorKind::UnexpectedFailure)?;
                Ok(())
            }
            _ => Err(SpecTestErrorKind::UnsupportedCommand)?,
        },

        CommandKind::AssertExhaustion { ref action } => match action {
            Action::Invoke {
                ref module,
                ref field,
                ref args,
            } => {
                let args = translate_args(args);
                let res = script.run(module, field, args);
                match res {
                    Ok(_) => Err(SpecTestErrorKind::UnexpectedSuccess)?,

                    Err(ScriptError::RuntimeError(RuntimeError::RuntimeFault(details))) => {
                        match details.trapcode.ty {
                            TrapCodeType::StackOverflow => Ok(()),
                            e => Err(format_err!(
                                "AssertExhaustion expects stack overflow, got {:?}",
                                e
                            )
                            .context(SpecTestErrorKind::UnexpectedFailure))?,
                        }
                    }

                    Err(e) => Err(Error::from(e).context(SpecTestErrorKind::UnexpectedFailure))?,
                }
            }
            _ => Err(SpecTestErrorKind::UnsupportedCommand)?,
        },

        CommandKind::AssertReturn {
            ref action,
            ref expected,
        } => match action {
            Action::Invoke {
                ref module,
                ref field,
                ref args,
            } => {
                let args = translate_args(args);
                let res = script
                    .run(module, field, args)
                    .context(SpecTestErrorKind::UnexpectedFailure)?;
                check_retval(expected, res)?;
                Ok(())
            }
            _ => Err(format_err!("non-invoke action"))
                .context(SpecTestErrorKind::UnsupportedCommand)?,
        },
        CommandKind::AssertReturnCanonicalNan { action }
        | CommandKind::AssertReturnArithmeticNan { action } => match action {
            Action::Invoke {
                ref module,
                ref field,
                ref args,
            } => {
                let args = translate_args(args);
                let res = script
                    .run(module, field, args)
                    .context(SpecTestErrorKind::UnexpectedFailure)?;
                let res_type = script
                    .instance_named(module)
                    .expect("just used that instance")
                    .type_of(field)
                    .expect("field has type");
                match res_type {
                    ExportType::Function(_, Some(ValueType::F32)) => {
                        if res.as_f32().is_nan() {
                            Ok(())
                        } else {
                            Err(format_err!("expected NaN, got {}", res.as_f32()))
                                .context(SpecTestErrorKind::IncorrectResult)?
                        }
                    }
                    ExportType::Function(_, Some(ValueType::F64)) => {
                        if res.as_f64().is_nan() {
                            Ok(())
                        } else {
                            Err(format_err!("expected NaN, got {}", res.as_f64()))
                                .context(SpecTestErrorKind::IncorrectResult)?
                        }
                    }
                    _ => Err(format_err!(
                        "expected a function returning point, got {:?}",
                        res_type
                    ))
                    .context(SpecTestErrorKind::UnexpectedFailure)?,
                }
            }
            _ => Err(format_err!("non-invoke action"))
                .context(SpecTestErrorKind::UnsupportedCommand)?,
        },
        CommandKind::AssertTrap { ref action, .. } => match action {
            Action::Invoke {
                module,
                field,
                args,
            } => {
                let args = translate_args(args);
                let res = script.run(module, field, args);
                match res {
                    Err(ScriptError::RuntimeError(_luceterror)) => Ok(()),
                    Err(e) => Err(Error::from(e).context(SpecTestErrorKind::UnexpectedFailure))?,
                    Ok(_) => Err(SpecTestErrorKind::UnexpectedSuccess)?,
                }
            }
            _ => Err(SpecTestErrorKind::UnsupportedCommand)?,
        },
    }
}

fn check_retval(expected: &[Value], got: UntypedRetVal) -> Result<(), SpecTestError> {
    match expected.len() {
        0 => {}
        1 => match expected[0] {
            Value::I32(expected) => {
                if expected != got.as_i32() {
                    Err(format_err!("expected {}, got {}", expected, got.as_i32()))
                        .context(SpecTestErrorKind::IncorrectResult)?
                }
            }
            Value::I64(expected) => {
                if expected != got.as_i64() {
                    Err(format_err!("expected {}, got {}", expected, got.as_i64()))
                        .context(SpecTestErrorKind::IncorrectResult)?
                }
            }
            Value::F32(expected) => {
                if expected != got.as_f32() && !expected.is_nan() && !got.as_f32().is_nan() {
                    Err(format_err!("expected {}, got {}", expected, got.as_f32()))
                        .context(SpecTestErrorKind::IncorrectResult)?
                }
            }
            Value::F64(expected) => {
                if expected != got.as_f64() && !expected.is_nan() && !got.as_f64().is_nan() {
                    Err(format_err!("expected {}, got {}", expected, got.as_f64()))
                        .context(SpecTestErrorKind::IncorrectResult)?
                }
            }
        },
        n => Err(format_err!("{} expected return values not supported", n))
            .context(SpecTestErrorKind::UnsupportedCommand)?,
    }
    Ok(())
}

fn translate_args(args: &[Value]) -> Vec<Val> {
    let mut out = Vec::new();
    for a in args {
        let v = match a {
            Value::I32(ref i) => Val::U32(*i as u32),
            Value::I64(ref i) => Val::U64(*i as u64),
            Value::F32(ref f) => Val::F32(*f),
            Value::F64(ref f) => Val::F64(*f),
        };
        out.push(v);
    }
    out
}
