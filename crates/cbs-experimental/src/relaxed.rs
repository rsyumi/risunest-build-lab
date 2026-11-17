use crate::cbs::{self, Budget, ChoiceTape, Context, Node, Units};
use serde::Serialize;

pub enum Tree {
    Output(Units),
    Choice(Vec<Tree>),
}
pub struct Program {
    pub tree: Tree,
    pub nodes: usize,
    pub leaves: usize,
    pub retained_bytes: usize,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Choice {
    pub arity: usize,
    pub index: usize,
}
pub struct Sample {
    pub output: Units,
    pub choices: Vec<Choice>,
    pub entropy_words: usize,
}
struct Limits {
    nodes: usize,
    leaves: usize,
    bytes: usize,
}
fn expand(
    nodes: &[Node],
    context: &Context,
    tape: &mut Vec<usize>,
    limits: &mut Limits,
    budget: &mut Budget,
) -> Result<Tree, String> {
    limits.nodes += 1;
    if limits.nodes > 512 || tape.len() > 12 {
        return Err("rng-tree-bound".into());
    }
    let mut current = context.clone();
    current.temp.clear();
    current.choices = Some(ChoiceTape {
        indices: tape.clone(),
        ..Default::default()
    });
    match cbs::evaluate(nodes, &mut current, budget) {
        Ok(output) => {
            limits.leaves += 1;
            limits.bytes += output.capacity() * 2 + std::mem::size_of::<Tree>();
            if limits.leaves > 256 || limits.bytes > 2_097_152 {
                return Err("rng-tree-bound".into());
            }
            Ok(Tree::Output(output))
        }
        Err(error) if error == "rng-choice-needed" => {
            let arity = current
                .choices
                .unwrap()
                .next_arity
                .ok_or("rng-choice-state")?;
            let mut branches = Vec::with_capacity(arity);
            limits.bytes += branches.capacity() * std::mem::size_of::<Tree>();
            for index in 0..arity {
                tape.push(index);
                let branch = expand(nodes, context, tape, limits, budget)?;
                tape.pop();
                branches.push(branch);
            }
            Ok(Tree::Choice(branches))
        }
        Err(error) => Err(error),
    }
}
pub fn prepare(source: &[u16], context: &Context) -> Result<Program, String> {
    if source.len() > 65_536 {
        return Err("rng-source-bound".into());
    }
    let nodes = cbs::compile(source)?;
    let mut limits = Limits {
        nodes: 0,
        leaves: 0,
        bytes: 0,
    };
    let mut budget = Budget {
        remaining: 1_000_000,
        max_output: 262_144,
        ..Default::default()
    };
    let tree = expand(&nodes, context, &mut Vec::new(), &mut limits, &mut budget)?;
    if limits.bytes > 2_097_152 {
        return Err("rng-tree-bound".into());
    }
    Ok(Program {
        tree,
        nodes: limits.nodes,
        leaves: limits.leaves,
        retained_bytes: limits.bytes,
    })
}
fn uniform_index(arity: usize, words: &mut usize) -> Result<usize, String> {
    let n = arity as u64;
    let threshold = n.wrapping_neg() % n;
    loop {
        let value = getrandom::u64().map_err(|_| "rng-entropy")?;
        *words += 1;
        if value >= threshold {
            return Ok((value % n) as usize);
        }
    }
}
pub fn sample(program: &Program, tape: Option<&[f64]>) -> Result<Sample, String> {
    if tape.is_some_and(|values| {
        values
            .iter()
            .any(|n| !n.is_finite() || *n < 0.0 || *n >= 1.0)
    }) {
        return Err("rng-tape-domain".into());
    }
    let mut node = &program.tree;
    let mut choices = Vec::new();
    let mut entropy_words = 0;
    loop {
        match node {
            Tree::Output(output) => {
                return Ok(Sample {
                    output: output.clone(),
                    choices,
                    entropy_words,
                })
            }
            Tree::Choice(branches) => {
                let index = if let Some(tape) = tape {
                    let unit = tape.get(choices.len()).ok_or("rng-tape-exhausted")?;
                    (*unit * branches.len() as f64).floor() as usize
                } else {
                    uniform_index(branches.len(), &mut entropy_words)?
                };
                choices.push(Choice {
                    arity: branches.len(),
                    index,
                });
                node = &branches[index];
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn correlated_failure_rejects_before_selection() {
        let source = cbs::units("{{{{random::getvar::upper}}::x}}");
        assert!(matches!(prepare(&source,&Context::default()),Err(e) if e=="unsupported-operator"));
    }
    #[test]
    fn one_choice_is_shared_and_invocations_are_fresh() {
        let source = cbs::units("{{settempvar::x::{{random::A::B}}}}{{tempvar::x}}|{{tempvar::x}}");
        let p = prepare(&source, &Context::default()).unwrap();
        let a = sample(&p, Some(&[0.1])).unwrap();
        let b = sample(&p, Some(&[0.9])).unwrap();
        assert_eq!(a.output, cbs::units("A|A"));
        assert_eq!(b.output, cbs::units("B|B"));
        assert_eq!(a.choices.len(), 1);
        assert_eq!(b.choices.len(), 1);
    }
    #[test]
    fn deterministic_forms_and_writes_are_rejected() {
        for source in [
            "{{pick::A::B}}",
            "{{rollp::2d6}}",
            "{{rollpick::2d6}}",
            "{{hash::x}}",
            "{{setvar::x::{{random::A::B}}}}",
        ] {
            assert!(prepare(&cbs::units(source), &Context::default()).is_err());
        }
    }
    #[test]
    fn runtime_failure_aborts_without_retry() {
        let p = prepare(&cbs::units("{{random::A::B}}"), &Context::default()).unwrap();
        assert!(matches!(sample(&p,Some(&[])),Err(e) if e=="rng-tape-exhausted"));
        assert!(matches!(sample(&p,Some(&[1.0])),Err(e) if e=="rng-tape-domain"));
    }
}
