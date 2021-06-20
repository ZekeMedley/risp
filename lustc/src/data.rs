//! Some constants that have values that can be determined at compile
//! time. Programs can then access them directly instead of needing to
//! do any work themselves. Herein lies the code for that.

use crate::compiler::{Context, JIT};
use crate::Expr;
use crate::PreorderStatus;
use crate::Word;

use cranelift::prelude::*;
use cranelift_module::Module;

impl Expr {
    /// A value is a complex constant if it appears inside of a quote
    /// expression. In that case we construct its value at compile time
    /// and store it in the programs data.
    pub fn is_complex_const(&self) -> Option<Word> {
        match self {
            Expr::List(v) => {
                if let Some(Expr::Symbol(s)) = v.first() {
                    if s == "quote" && v.len() == 2 {
                        Some(v[1].word_rep())
                    } else {
                        None
                    }
                } else {
                    None
                }
            }
            Expr::String(_) => Some(self.word_rep()),
            _ => None,
        }
    }
}

/// Information about data that will be compiled into the program's
/// data section.
#[derive(Debug)]
pub struct LustData {
    /// The name of the data. Tends to be __anon_data_<num>
    pub name: String,
    /// The data itself. This is generated by
    /// Expr::to_immediate. Ownership of this data is given to the
    /// program.
    pub data: Word,
}

fn collect_data_w_count(program: &[Expr], count: &mut usize) -> Vec<LustData> {
    let mut res = Vec::new();

    for e in program {
        e.preorder_traverse(&mut |e: &Expr| {
            if let Some((_, args)) = e.is_foreign_call() {
                res.extend(collect_data_w_count(args, count));
                return PreorderStatus::Skip;
            } else if let Some(repr) = e.is_complex_const() {
                res.push(LustData {
                    name: format!("__anon_data_{}", count),
                    data: repr,
                });
                *count += 1;
            }
            PreorderStatus::Continue
        });
    }

    res
}

/// Collects all of the complex constants in the program and marshals
/// them into a list.
pub(crate) fn collect_data(program: &[Expr]) -> Vec<LustData> {
    let _t = crate::timer::timeit("data collection pass");
    let mut count = 0;
    collect_data_w_count(program, &mut count)
}

fn replace_data_w_count(program: &mut [Expr], data: &[LustData], count: &mut usize) {
    for e in program {
        e.preorder_traverse_mut(&mut |e: &mut Expr| {
            if let Some((_, args)) = e.is_foreign_call_mut() {
                replace_data_w_count(args, data, count);
                return PreorderStatus::Skip;
            } else if let Some(_) = e.is_complex_const() {
                *e = Expr::Symbol(data[*count].name.clone());
                *count += 1;
            }
            PreorderStatus::Continue
        });
    }
}

pub(crate) fn emit_data_access(name: &str, ctx: &mut Context) -> Result<Value, String> {
    let sym = ctx
        .module
        .declare_data(name, cranelift_module::Linkage::Export, true, false)
        .map_err(|e| e.to_string())?;
    let local_id = ctx.module.declare_data_in_func(sym, ctx.builder.func);

    let data_ptr = ctx.builder.ins().symbol_value(ctx.wordtype, local_id);
    Ok(ctx
        .builder
        .ins()
        .load(ctx.reftype, MemFlags::new(), data_ptr, 0))
}

/// Replaces all of the complex constants in the program with a symbol
/// that when looked up yields the data that it once represented. For
/// example, the program:
///
/// ```lisp
/// (let a (quote (1 2 3)))
/// ```
///
/// Is transformed into:
///
/// ```lisp
/// (let a __anon_data_0)
/// ```
///
/// by this pass.
pub(crate) fn replace_data(program: &mut [Expr], data: &[LustData]) {
    let _t = crate::timer::timeit("data replacement pass");
    let mut count = 0;
    replace_data_w_count(program, data, &mut count);
}

/// Gives ownership of DATA to JIT and assocaites its name with its
/// value internally.
pub(crate) fn create_data(data: LustData, jit: &mut JIT) -> Result<(), String> {
    let contents = Box::new(data.data.to_ne_bytes());
    jit.data_ctx.define(contents);
    let id = jit
        .module
        .declare_data(&data.name, cranelift_module::Linkage::Export, true, false)
        .map_err(|e| e.to_string())?;

    jit.module
        .define_data(id, &jit.data_ctx)
        .map_err(|e| e.to_string())?;

    jit.data_ctx.clear();

    // NOTE: this is only safe to do so long as data processing comes
    // before function processing.
    jit.module.finalize_definitions();

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse_string;
    use crate::roundtrip_file;
    use crate::roundtrip_string;

    #[test]
    fn test_data_collection() {
        let source = r#"
(let foo (quote (1 2 3)))

(if 1 (quote (1 2)) (quote (2 3)))
"#;
        let exprs = parse_string(source).unwrap();

        let data = collect_data(&exprs);

        assert_eq!(data.len(), 3);

        assert_eq!(
            Expr::from_immediate(data[2].data),
            Expr::List(vec![
                Expr::Integer(2),
                Expr::List(vec![Expr::Integer(3), Expr::Nil])
            ])
        )
    }

    #[test]
    fn test_data() {
        let expected_source = r#"
(cons (eq 1 1) (cons 1 (cons 1 ())))
"#;
        let expected = roundtrip_string(expected_source).unwrap();
        let res = roundtrip_file("examples/data.lisp").unwrap();
        assert_eq!(expected, res)
    }
}
