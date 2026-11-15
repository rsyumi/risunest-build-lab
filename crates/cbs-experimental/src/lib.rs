mod arithmetic;
pub mod cbs;
pub mod relaxed;

#[cfg(test)]
mod tests {
    use super::cbs::{self, evaluate, Budget, Context};
    #[test]
    fn cancellation_is_cooperative_and_discards_local_state() {
        let flag = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let cancel = flag.clone();
        let handle = std::thread::spawn(move || {
            let program =
                vec![cbs::Node::Macro(vec![cbs::Node::Text(cbs::units("random::a::b"))]); 200_000];
            let mut c = Context::default();
            let mut b = Budget {
                cancel: flag,
                ..Budget::default()
            };
            evaluate(&program, &mut c, &mut b)
        });
        cancel.store(true, std::sync::atomic::Ordering::Relaxed);
        assert_eq!(handle.join().unwrap().unwrap_err(), "cancelled");
    }
    #[test]
    fn cancellation_of_a_running_evaluator() {
        let flag = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let cancel = flag.clone();
        let progress = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let child_progress = progress.clone();
        let handle = std::thread::spawn(move || {
            let p = vec![cbs::Node::Text(vec![65]); 400_000];
            let mut c = Context::default();
            let mut b = Budget {
                cancel: flag,
                progress: child_progress,
                ..Budget::default()
            };
            evaluate(&p, &mut c, &mut b)
        });
        while progress.load(std::sync::atomic::Ordering::Relaxed) < 100 {
            std::thread::yield_now()
        }
        cancel.store(true, std::sync::atomic::Ordering::Relaxed);
        assert_eq!(handle.join().unwrap().unwrap_err(), "cancelled");
    }

    #[test]
    fn replace_preserves_string_substitutions_and_missing_arguments() {
        for (source, expected) in [
            ("{{replace::ababa::aba::X}}", "Xba"),
            ("{{replace::abc::b::$$|$&|$`|$'|$1|$99|$<x>}}", "a$|b|a|c|$1|$99|$<x>c"),
            ("{{replace::aa::a::$`-$'}}", "-aa-"),
            ("{{replace::ab::::_}}", "_a_b_"),
            ("{{replace::::::_}}", "_"),
            ("{{replace::abc::z::x}}", "abc"),
            ("{{replace::undefined}}", "undefined"),
            ("{{replace::undefined::undefined}}", "undefined"),
            ("{{replace::undefined::undefined::}}", ""),
            ("{{replace::abc::b}}", "aundefinedc"),
            ("{{replace}}", "{{replace}}"),
            ("{{#if 0}}{{replace::x::x::y}}{{/if}}", ""),
            ("{{#if 1}}{{replace::x::x::y}}{{/if}}", "y"),
            ("{{#when 0}}{{replace::x::x::{{settempvar::x::yes}}}}{{/when}}{{gettempvar::x}}", "yes"),
        ] {
            let program = cbs::compile(&cbs::units(source)).unwrap();
            for fold in [false, true] {
                let mut program = program.clone();
                if fold {
                    cbs::fold_constants(&mut program);
                }
                let output = evaluate(&program, &mut Context::default(), &mut Budget::default()).unwrap();
                assert_eq!(output, cbs::units(expected), "{source}, fold={fold}");
            }
        }
        let mut literal = cbs::compile(&cbs::units("{{replace::abc::b::x}}")).unwrap();
        assert_eq!(cbs::fold_constants(&mut literal), 1);
    }
    #[test]
    fn replace_uses_utf16_boundaries_and_defers_generated_cbs() {
        let source = cbs::units("{{replace::{{getvar::value}}::::_}}");
        let program = cbs::compile(&source).unwrap();
        let mut context = Context::default();
        context.variables.insert(cbs::units("value"), vec![0xd83d, 0xde00, 0xd800]);
        assert_eq!(evaluate(&program, &mut context, &mut Budget::default()).unwrap(),
            vec![95, 0xd83d, 95, 0xde00, 95, 0xd800, 95]);
        context.user = cbs::units("Synthetic");
        context.variables.insert(cbs::units("next"), cbs::units("{{user}}"));
        let program = cbs::compile(&cbs::units("{{replace::x::x::{{getvar::next}}}}")).unwrap();
        let output = evaluate(&program, &mut context, &mut Budget::default()).unwrap();
        assert_eq!(output, cbs::units("{{user}}"));
        let next = cbs::compile(&output).unwrap();
        assert_eq!(evaluate(&next, &mut context, &mut Budget::default()).unwrap(), cbs::units("Synthetic"));
    }
    #[test]
    fn replace_enforces_output_and_work_budgets() {
        let program = cbs::compile(&cbs::units("prefix-text{{replace::aaa::a::0123456789}}")).unwrap();
        let mut budget = Budget { max_output: 40, remaining: 10, ..Budget::default() };
        assert_eq!(evaluate(&program, &mut Context::default(), &mut budget).unwrap_err(), "output-limit");
        let program = cbs::compile(&cbs::units("{{replace::aaaaaaaa::a::$`$`$`$`$'}}")).unwrap();
        let mut budget = Budget { max_output: 40, ..Budget::default() };
        assert_eq!(evaluate(&program, &mut Context::default(), &mut budget).unwrap_err(), "output-limit");
        let program = cbs::compile(&cbs::units(&format!("{{{{replace::{}::ab::}}}}", "ab".repeat(8_192)))).unwrap();
        let mut budget = Budget { remaining: 8, ..Budget::default() };
        assert_eq!(evaluate(&program, &mut Context::default(), &mut budget).unwrap_err(), "budget");
    }
    #[test]
    fn replacement_can_be_cancelled_while_searching() {
        let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let progress = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let child_cancel = cancel.clone();
        let child_progress = progress.clone();
        let source = cbs::units(&format!("{{{{replace::{}::b::x}}}}", "a".repeat(2_000_000)));
        let program = cbs::compile(&source).unwrap();
        let handle = std::thread::spawn(move || {
            let mut budget = Budget { cancel: child_cancel, progress: child_progress, ..Budget::default() };
            evaluate(&program, &mut Context::default(), &mut budget)
        });
        while progress.load(std::sync::atomic::Ordering::Relaxed) < 16 {
            assert!(!handle.is_finished(), "replacement finished before cancellation checkpoint");
            std::thread::yield_now();
        }
        cancel.store(true, std::sync::atomic::Ordering::Relaxed);
        assert_eq!(handle.join().unwrap().unwrap_err(), "cancelled");
    }

    #[test]
    fn hash_preserves_missing_arguments_and_literal_folding() {
        for source in ["{{hash}}", "{{HASH}}", "{{ha_sh}}"] {
            assert_array_output(source, source);
        }
        for source in ["{{hash::}}", "{{HASH:}}", "{{ha_sh::}}", "{{#if 1}}{{hash::}}{{/}}"] {
            assert_array_output(source, "0000040");
        }
        assert_array_output("{{#if 0}}{{hash::{{settempvar::x::yes}}}}{{/}}{{gettempvar::x}}", "");
        let mut program = cbs::compile(&cbs::units("{{hash::}}")).unwrap();
        assert_eq!(cbs::fold_constants(&mut program), 1);
        assert_eq!(evaluate(&program, &mut Context::default(), &mut Budget::default()).unwrap(), cbs::units("0000040"));
        let program = cbs::compile(&cbs::units("{{hash::}}")).unwrap();
        let mut budget = Budget { max_output: 6, ..Budget::default() };
        assert_eq!(evaluate(&program, &mut Context::default(), &mut budget).unwrap_err(), "output-limit");
    }

    #[test]
    fn scalar_numbers_preserve_coercion_rounding_and_fixed_decimal_output() {
        for (source, expected) in [
            ("{{round}}", "NaN"), ("{{round::}}", "0"),
            ("{{round::-0.5}}", "0"), ("{{round::-1.5}}", "-1"),
            ("{{round::0.49999999999999994}}", "0"),
            ("{{round::4503599627370496}}", "4503599627370496"),
            ("{{floor::-1.1}}", "-2"), ("{{ceil::-1.1}}", "-1"),
            ("{{abs::-Infinity}}", "Infinity"), ("{{abs::-0}}", "0"),
            ("{{remaind::-7::3}}", "-1"), ("{{remaind::7::-3}}", "1"),
            ("{{remaind::1::0}}", "NaN"), ("{{remaind::1::Infinity}}", "1"),
            ("{{fixnum}}", "NaN"), ("{{fixnum::}}", "0"),
            ("{{fixnum::1.005::2}}", "1.00"), ("{{fixnum::2.55::1}}", "2.5"),
            ("{{fixnumber::-0.125::2}}", "-0.13"), ("{{fixnum::0.125::2}}", "0.13"),
            ("{{fixnum::-0::2}}", "0.00"), ("{{fixnum::-0.0001::2}}", "-0.00"),
            ("{{fixnum::1000000000000000128::0}}", "1000000000000000128"),
            ("{{fixnum::1e21::2}}", "1e+21"), ("{{fixnum::1.25::1.9}}", "1.3"),
            ("{{fixnum::1.5::-0.9}}", "2"), ("{{fixnum::NaN::101}}", "{{fixnum::NaN::101}}"),
            ("{{fixnum::Infinity::-1}}", "{{fixnum::Infinity::-1}}"),
            ("{{fixnum::Infinity::NaN}}", "Infinity"),
            ("{{abs::0x20000000000001}}", "9007199254740992"),
            ("{{abs::0x20000000000003}}", "9007199254740996"),
            ("{{less::0x20000000000001::9007199254740993}}", "0"),
            ("{{arraysplice::[1,2]::0xFFFFFFFFFFFFFFFFFFFFFFFF::0::x}}", "[1,2,\"x\"]"),
            ("{{floor::+0x10}}", "NaN"), ("{{floor::1e}}", "NaN"),
            ("{{floor::\u{feff} 1.9 \u{a0}}}", "1"),
        ] {
            assert_array_output(source, expected);
        }
        assert_array_output("{{fixnum::0.125::100}}", &("0.125".to_owned() + &"0".repeat(97)));
        assert_array_output("{{fixnum::-5e-324::100}}", &("-0.".to_owned() + &"0".repeat(100)));
        assert_array_output(&format!("{{{{abs::0x{}}}}}", "f".repeat(256)), "Infinity");
        assert_array_output(&format!("{{{{abs::0x{}z}}}}", "f".repeat(256)), "NaN");
    }

    #[test]
    fn scalar_utf16_and_character_filter_preserve_javascript_behavior() {
        for (source, expected) in [
            ("{{tonumber}}", "{{tonumber}}"), ("{{unicodeencode}}", "{{unicodeencode}}"),
            ("{{tonumber::x-1.2e3 +\u{feff}\u{a0}４😀}}", "1.23 \u{feff}\u{a0}"),
            ("{{unicodeencode::😀::0}}", "55357"), ("{{unicode_encode::😀::1}}", "56832"),
            ("{{unicodeencode::A::-0.9}}", "65"), ("{{unicodeencode::A::NaN}}", "65"),
            ("{{unicodeencode::A::Infinity}}", "NaN"), ("{{unicodeencode::}}", "NaN"),
            ("{{unicodedecode::65537.9}}", "\u{1}"),
            ("{{u::+0x41trailing}}", "A"), ("{{ue::41}}", "A"),
            ("{{unicodedecodefromhex::1000000000000081}}", "\u{100}"),
            ("{{unicodeencodefromhex::-0x1000000000000081tail}}", "\u{ff00}"),
        ] {
            assert_array_output(source, expected);
        }
        for (source, expected) in [
            ("{{unicodedecode}}", vec![0]), ("{{unicode_decode::NaN}}", vec![0]),
            ("{{u::}}", vec![0]), ("{{ue::Infinity}}", vec![0]),
            ("{{unicodedecode::-1.9}}", vec![65535]), ("{{u::d800}}", vec![0xd800]),
            ("{{unicode_decode::56320}}", vec![0xdc00]),
        ] {
            let program = cbs::compile(&cbs::units(source)).unwrap();
            for fold in [false, true] {
                let mut program = program.clone();
                if fold { cbs::fold_constants(&mut program); }
                assert_eq!(evaluate(&program, &mut Context::default(), &mut Budget::default()).unwrap(), expected);
            }
        }
        assert_array_output("{{unicodeencode::{{u::d800}}}}", "55296");
        assert_array_output("{{#if 0}}{{round::{{settempvar::x::yes}}}}{{/}}{{gettempvar::x}}", "");
        assert_array_output("{{#when 0}}{{round::{{settempvar::x::yes}}}}{{/}}{{gettempvar::x}}", "yes");
        let program = cbs::compile(&cbs::units("{{pow::2::3}}")).unwrap();
        assert_eq!(evaluate(&program, &mut Context::default(), &mut Budget::default()).unwrap_err(), "unsupported-operator");
    }

    #[test]
    fn scalar_work_output_and_cancellation_are_bounded() {
        let program = cbs::compile(&cbs::units("{{fixnum::0.125::100}}")).unwrap();
        let mut budget = Budget { remaining: 20, ..Budget::default() };
        assert_eq!(evaluate(&program, &mut Context::default(), &mut budget).unwrap_err(), "budget");
        let mut budget = Budget { max_output: 40, ..Budget::default() };
        assert_eq!(evaluate(&program, &mut Context::default(), &mut budget).unwrap_err(), "output-limit");
        let mut budget = Budget { max_output: 102, ..Budget::default() };
        assert_eq!(evaluate(&program, &mut Context::default(), &mut budget).unwrap(),
            cbs::units(&("0.125".to_owned() + &"0".repeat(97))));
        let program = cbs::compile(&cbs::units(&format!("{{{{round::0x{}}}}}", "f".repeat(8192)))).unwrap();
        let mut budget = Budget { remaining: 8, ..Budget::default() };
        assert_eq!(evaluate(&program, &mut Context::default(), &mut budget).unwrap_err(), "budget");
        let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let progress = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let child_cancel = cancel.clone();
        let child_progress = progress.clone();
        let source = cbs::units(&format!("{{{{tonumber::{}}}}}", "1".repeat(2_000_000)));
        let program = cbs::compile(&source).unwrap();
        let handle = std::thread::spawn(move || {
            let mut budget = Budget { cancel: child_cancel, progress: child_progress, ..Budget::default() };
            evaluate(&program, &mut Context::default(), &mut budget)
        });
        while progress.load(std::sync::atomic::Ordering::Relaxed) < 16 {
            assert!(!handle.is_finished(), "scalar finished before cancellation checkpoint");
            std::thread::yield_now();
        }
        cancel.store(true, std::sync::atomic::Ordering::Relaxed);
        assert_eq!(handle.join().unwrap().unwrap_err(), "cancelled");
    }

    #[test]
    fn aggregates_preserve_argument_selection_json_types_and_sequential_arithmetic() {
        for (source, expected) in [
            ("{{min::[]}}", "Infinity"), ("{{max::[]}}", "-Infinity"),
            ("{{sum::[]}}", "0"), ("{{average::[]}}", "NaN"),
            ("{{min::}}", "0"), ("{{max::-0::0}}", "0"),
            ("{{sum::true}}", "0"), ("{{sum::[true]}}", "1"),
            (r#"{{sum::"2"}}"#, "0"), (r#"{{sum::["2"]}}"#, "2"),
            ("{{sum::[1,2]::3}}", "3"), ("{{average::[1,2]::3}}", "1.5"),
            ("{{sum::1§2§bad}}", "3"), ("{{sum::1§2::4}}", "4"),
            ("{{min::bad::5}}", "0"), ("{{max::bad::-5}}", "0"),
            ("{{average::bad::2}}", "1"),
            (r#"{{sum::[true,false,null,"2","bad",{},[],[3],[1,2]]}}"#, "6"),
            (r#"{{sum::[["2"],[null],[true],[],[[3]],{}]}}"#, "5"),
            (r#"{{sum::[{"valueOf":7},{"__proto__":{"toString":null} },{"x":{"toString":0} }]}}"#, "0"),
            (r#"{{sum::[{"x":{"toString":0}}]}}"#, "0]}}"),
            (r#"{{sum::{"toString":null}}}"#, "0}"),
            ("{{sum::Infinity::-Infinity::4}}", "NaN"),
            ("{{average::Infinity::-Infinity::4}}", "NaN"),
            ("{{sum::[1e400,-1e400]}}", "NaN"),
            ("{{sum::[1e16,1,-1e16]}}", "0"), ("{{sum::[1e16,-1e16,1]}}", "1"),
            ("{{average::[1e16,1,-1e16]}}", "0"),
            ("{{average::[-5e-324,0]}}", "0"), ("{{sum::[1e21,1e-7]}}", "1e+21"),
        ] { assert_array_output(source, expected); }
        for (op, expected) in [("min", "1"), ("max", "2"), ("sum", "3"), ("average", "1.5")] {
            let mut program = cbs::compile(&cbs::units(&format!("{{{{{op}::[1,2]}}}}"))).unwrap();
            assert_eq!(cbs::fold_constants(&mut program), 1, "{op} literal was not folded");
            assert_eq!(evaluate(&program, &mut Context::default(), &mut Budget::default()).unwrap(), cbs::units(expected));
        }
        for op in ["min", "max", "sum", "average"] {
            let source = format!("{{{{{op}}}}}");
            assert_array_output(&source, &source);
            for input in [r#"[{"toString":null}]"#, r#"[[{"toString":0}]]"#, r#"[{"valueOf":1,"toString":true}]"#] {
                let source = format!("{{{{{op}::{input}}}}}");
                assert_array_output(&source, &source);
            }
        }
        assert_array_output("{{#if 0}}{{sum::{{settempvar::x::yes}}}}{{/}}{{gettempvar::x}}", "");
        assert_array_output("{{#when 0}}{{sum::{{settempvar::x::yes}}}}{{/}}{{gettempvar::x}}", "yes");
    }

    #[test]
    fn aggregate_spread_bound_does_not_limit_sum_or_average() {
        let boundary = vec!["1"; 8192];
        let above = vec!["1"; 8193];
        for op in ["min", "max"] {
            for input in [format!("[{}]", boundary.join(",")), boundary.join("::")] {
                assert_array_output(&format!("{{{{{op}::{input}}}}}"), "1");
            }
            for input in [format!("[{}]", above.join(",")), above.join("::")] {
                let program = cbs::compile(&cbs::units(&format!("{{{{{op}::{input}}}}}"))).unwrap();
                assert_eq!(evaluate(&program, &mut Context::default(), &mut Budget::default()).unwrap_err(), "aggregate-spread-limit");
            }
        }
        for (op, expected) in [("sum", "8193"), ("average", "1")] {
            for input in [format!("[{}]", above.join(",")), above.join("::")] {
                assert_array_output(&format!("{{{{{op}::{input}}}}}"), expected);
            }
        }
    }

    #[test]
    fn aggregate_work_intermediates_depth_and_cancellation_are_bounded() {
        let source = format!("{{{{sum::[\"{}\"]}}}}", "1".repeat(8192));
        let program = cbs::compile(&cbs::units(&source)).unwrap();
        let mut budget = Budget { remaining: 8, ..Budget::default() };
        assert_eq!(evaluate(&program, &mut Context::default(), &mut budget).unwrap_err(), "budget");
        let program = cbs::compile(&cbs::units("{{sum::[[{},{}]]}}")).unwrap();
        let mut budget = Budget { max_output: 20, ..Budget::default() };
        assert_eq!(evaluate(&program, &mut Context::default(), &mut budget).unwrap_err(), "output-limit");
        let mut budget = Budget { max_output: 31, ..Budget::default() };
        assert_eq!(evaluate(&program, &mut Context::default(), &mut budget).unwrap(), cbs::units("0"));
        let program = cbs::compile(&cbs::units(&format!("{{{{sum::{}1{}}}}}", "[".repeat(65), "]".repeat(65)))).unwrap();
        assert_eq!(evaluate(&program, &mut Context::default(), &mut Budget::default()).unwrap_err(), "json-depth");
        let source = format!("{{{{sum::{}}}}}", vec!["1"; 300_000].join("::"));
        let program = cbs::compile(&cbs::units(&source)).unwrap();
        let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let progress = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let child_cancel = cancel.clone();
        let child_progress = progress.clone();
        let handle = std::thread::spawn(move || {
            let mut budget = Budget { cancel: child_cancel, progress: child_progress, ..Budget::default() };
            evaluate(&program, &mut Context::default(), &mut budget)
        });
        while progress.load(std::sync::atomic::Ordering::Relaxed) < 16 {
            assert!(!handle.is_finished(), "aggregate finished before cancellation checkpoint");
            std::thread::yield_now();
        }
        cancel.store(true, std::sync::atomic::Ordering::Relaxed);
        assert_eq!(handle.join().unwrap().unwrap_err(), "cancelled");
    }

    fn assert_array_output(source: &str, expected: &str) {
        let program = cbs::compile(&cbs::units(source)).unwrap();
        for fold in [false, true] {
            let mut program = program.clone();
            if fold {
                cbs::fold_constants(&mut program);
            }
            assert_eq!(evaluate(&program, &mut Context::default(), &mut Budget::default()).unwrap(),
                cbs::units(expected), "{source}, fold={fold}");
        }
    }
    #[test]
    fn array_operations_preserve_typed_json_and_top_level_string_escaping() {
        for (source, expected) in [
            (r#"{{arrayshift::[0,true,null,[1,2],{"2":0,"1":1},"a\u003a\u003ab"]}}"#,
                r#"[true,null,[1,2],{"1":1,"2":0},"a\\u003A\\u003Ab"]"#),
            (r#"{{arraypop::[["a\u003a\u003ab"],{"value":"c\u003a\u003ad"},"discard"]}}"#,
                r#"[["a::b"],{"value":"c::d"}]"#),
            (r#"{{arraypush::[1,true,null]::false}}"#, r#"[1,true,null,"false"]"#),
            (r#"{{arraypush::[1e400,-0,{"x":1,"x":2}]}}"#, r#"[null,0,{"x":2},null]"#),
            (r#"{{arrayshift::[0,"\ud800","\udc00","\ud83d\ude00"]}}"#, r#"["\ud800","\udc00","😀"]"#),
            ("{{arrayshift::a§b§c}}", r#"["b","c"]"#),
            ("{{arraypop::a§b§c}}", r#"["a","b"]"#),
            ("{{arraypush::[]}}", "[null]"),
            ("{{arraypush::}}", r#"["",null]"#),
            ("{{arraypop::null}}", "[]"),
            (r#"{{arraypush::{"a":1}::x}}"#, r#"["{\"a\":1}","x"]"#),
            ("{{arraypush::[]::2::ignored}}", r#"["2"]"#),
            (r#"{{arraypush::["\u+041"]::x}}"#, r#"["[\"\\u+041\"]","x"]"#),
            (r#"{{arrayelement::["\u+041"]::0}}"#, r#"["\u+041"]"#),
        ] {
            assert_array_output(source, expected);
        }
        for op in ["arrayshift", "arraypop", "arraypush", "arraysplice"] {
            let source = format!("{{{{{op}}}}}");
            assert_array_output(&source, &source);
        }
    }
    #[test]
    fn arraysplice_converts_indices_and_always_inserts_one_value() {
        for (arguments, expected) in [
            ("", "[null,1,2,3]"),
            ("::1", "[1,null,2,3]"),
            ("::1::1", "[1,null,3]"),
            ("::-1.9::1.9::x", r#"[1,2,"x"]"#),
            ("::-Infinity::Infinity::x", r#"["x"]"#),
            ("::Infinity::Infinity::x", r#"[1,2,3,"x"]"#),
            ("::-0::-Infinity::x", r#"["x",1,2,3]"#),
            ("::NaN::NaN::x", r#"["x",1,2,3]"#),
            ("::::::x", r#"["x",1,2,3]"#),
            ("::0x1::0b10::x", r#"[1,"x"]"#),
            ("::-999::999::x", r#"["x"]"#),
        ] {
            assert_array_output(&format!("{{{{arraysplice::[1,2,3]{arguments}}}}}"), expected);
        }
    }
    #[test]
    fn array_operations_preserve_invocation_and_depth_contracts() {
        assert_array_output("{{#if 0}}{{arraypush::[]::{{settempvar::x::yes}}}}{{/if}}{{gettempvar::x}}", "");
        assert_array_output("{{#when 0}}{{arraypush::[]::{{settempvar::x::yes}}}}{{/when}}{{gettempvar::x}}", "yes");
        assert_array_output("{{#if 1}}{{arraypop::[1,2]}}{{/if}}", "[1]");
        let mut context = Context::default();
        context.variables.insert(cbs::units("input"), cbs::units("[1,2]"));
        context.variables.insert(cbs::units("op"), cbs::units("arrayshift"));
        let source = "{{{{getvar::op}}::{{getvar::input}}}}{{arrayshift::{{getvar::input}}}}";
        let program = cbs::compile(&cbs::units(source)).unwrap();
        assert_eq!(evaluate(&program, &mut context, &mut Budget::default()).unwrap(), cbs::units("[2][2]"));
        assert_eq!(context.variables[&cbs::units("input")], cbs::units("[1,2]"));
        for op in ["arrayshift", "arraypop", "arraypush", "arraysplice"] {
            let source = format!("{{{{{op}::{}0{}}}}}", "[".repeat(65), "]".repeat(65));
            let program = cbs::compile(&cbs::units(&source)).unwrap();
            assert_eq!(evaluate(&program, &mut Context::default(), &mut Budget::default()).unwrap_err(), "json-depth");
        }
    }

    fn effect_input(entries: &[(&str, &str)]) -> cbs::compat::Input {
        use cbs::compat::{Cell, Input, Value};
        Input { run_var: true, variables: entries.iter().map(|(k, v)|
            (cbs::units(k), Cell::Value(Value::Text(cbs::units(v))))).collect(), ..Input::default() }
    }
    fn effect_eval(source: &str, input: &cbs::compat::Input) -> cbs::compat::Evaluation {
        let program = cbs::compat::compile(&cbs::units(source)).unwrap();
        match cbs::compat::evaluate(&program, input, &mut Budget::default()) {
            cbs::compat::Outcome::Evaluated(value) => value,
            other => panic!("{source}: {other:?}"),
        }
    }
    fn effect_text(value: &str) -> cbs::compat::Value { cbs::compat::Value::Text(cbs::units(value)) }

    fn history_input(length: i64, rows: &[(i64, &str, &str)]) -> cbs::compat::Input {
        use cbs::history::{Limits, PreparedAbsoluteHistory, Provenance, Row};
        let mut input = effect_input(&[]);
        input.context.index = -1;
        input.history = Some(PreparedAbsoluteHistory {
            length, rows: rows.iter().map(|&(index, data, role)| Row { index, data: cbs::units(data), role: cbs::units(role) }).collect(),
            selected_character: true, condition_chat_role: None, firstmsg: false, role: None,
            provenance: Provenance { capture_id: cbs::units("capture"), character_id: cbs::units("character"),
                conversation_id: cbs::units("conversation"), revision: Some(u64::MAX) },
            limits: Limits { rows: 2048, input_units: 1_000_000, retained_bytes: 4_000_000, dependencies: 2048 },
        });
        input
    }
    fn history_eval(source: &str, input: &cbs::compat::Input) -> cbs::compat::Evaluation {
        let program = cbs::compat::compile_with_history(&cbs::units(source)).unwrap();
        match cbs::compat::evaluate(&program, input, &mut Budget::default()) {
            cbs::compat::Outcome::Evaluated(value) => value,
            other => panic!("{source}: {other:?}"),
        }
    }

    #[test]
    fn history_sparse_total_and_absolute_indices_are_not_loaded_positions() {
        let input = history_input(10, &[(3, "absolute-three", "user"), (9, "tail", "char")]);
        let result = history_eval("{{lastmessageid}}/{{lastmessageindex}}/{{lastmessage}}/{{previous_chat_log::3}}", &input);
        assert_eq!(result.result, Ok(cbs::units("9/9/tail/absolute-three")));
        let observation = result.history.unwrap();
        assert!(observation.count);
        assert_eq!(observation.rows, vec![9, 3]);
        assert_eq!(observation.provenance, input.history.as_ref().unwrap().provenance);
        assert!(observation.affected_by(0, 0, true));
        assert!(observation.affected_by(3, 4, false));
        assert!(!observation.affected_by(4, 9, false));
        assert_eq!(history_eval("{{previouschatlog::3}}{{previouschatlog::9}}{{previouschatlog::3}}", &input).history.unwrap().rows, vec![3, 9]);
    }

    #[test]
    fn history_number_conversion_empty_rows_and_selection_remain_distinct() {
        let input = history_input(2, &[(0, "zero", "user"), (1, "", "char")]);
        for (source, expected) in [("{{previouschatlog}}", "Out of range"), ("{{previouschatlog::}}", "zero"),
            ("{{previouschatlog::-0}}", "zero"), ("{{previouschatlog::0x0}}", "zero"),
            ("{{previouschatlog::0b0}}", "zero"), ("{{previouschatlog::  }}", "zero"),
            ("{{previouschatlog::1}}", ""), ("{{previouschatlog::0.5}}", "Out of range"),
            ("{{previouschatlog::-1}}", "Out of range"), ("{{previouschatlog::Infinity}}", "Out of range"),
            ("{{previouschatlog::-Infinity}}", "Out of range"), ("{{previouschatlog::NaN}}", "Out of range"),
            ("{{previouschatlog::undefined}}", "Out of range")] {
            assert_eq!(history_eval(source, &input).result, Ok(cbs::units(expected)), "{source}");
        }
        let mut empty = history_input(0, &[]);
        let result = history_eval("{{lastmessage}}/{{lastmessageid}}/{{previouschatlog::0}}", &empty);
        assert_eq!(result.result, Ok(cbs::units("{{lastmessage}}/-1/Out of range")));
        assert!(result.history.unwrap().count);
        empty.history.as_mut().unwrap().selected_character = false;
        let result = history_eval("{{lastmessage}}{{lastmessageid}}", &empty);
        assert_eq!(result.result, Ok(Vec::new()));
        assert!(!result.history.unwrap().count);
        let mut input = input;
        input.history.as_mut().unwrap().selected_character = false;
        input.context.index = 0;
        assert_eq!(history_eval("{{previouschatlog::0}}/{{role}}", &input).result, Ok(cbs::units("zero/user")));
    }

    #[test]
    fn history_role_precedence_includes_empty_nullish_fallback() {
        let mut input = history_input(2, &[(1, "row", "user")]);
        input.context.index = 1;
        input.history.as_mut().unwrap().firstmsg = true;
        input.history.as_mut().unwrap().condition_chat_role = Some(cbs::units("system"));
        let result = history_eval("{{role}}", &input);
        assert_eq!(result.result, Ok(cbs::units("system")));
        assert!(!result.history.unwrap().count);
        input.history.as_mut().unwrap().condition_chat_role = Some(Vec::new());
        assert_eq!(history_eval("{{role}}", &input).result, Ok(cbs::units("char")));
        input.history.as_mut().unwrap().firstmsg = false;
        assert_eq!(history_eval("{{role}}", &input).result, Ok(cbs::units("user")));
        input.context.index = -1;
        assert_eq!(history_eval("{{role}}", &input).result, Ok(cbs::units("null")));
        input.history.as_mut().unwrap().role = Some(Vec::new());
        assert_eq!(history_eval("{{role}}", &input).result, Ok(Vec::new()));
        input.context.index = 2;
        let result = history_eval("{{setvar::a::1}}{{role}}{{setvar::b::2}}", &input);
        assert_eq!(result.result, Ok(cbs::units("{{role}}")));
        assert_eq!(result.effects.len(), 2);
    }

    #[test]
    fn history_missing_row_keeps_exact_effect_prefix_and_dependencies() {
        use cbs::compat::{ReadError, Value};
        let input = history_input(10, &[(3, "known", "user")]);
        let result = history_eval("{{setvar::score::changed}}{{lastmessage}}{{setvar::later::no}}", &input);
        assert_eq!(result.result, Err(ReadError::HistoryAccess(cbs::units("row 9"))));
        assert_eq!(result.effects, vec![cbs::compat::Write { sequence: 0, key: effect_text("score"), value: effect_text("changed") }]);
        assert_eq!(result.overlay, vec![(cbs::units("score"), effect_text("changed"))]);
        assert_eq!(result.history.unwrap().rows, vec![9]);
        let result = history_eval("{{previouschatlog::{{setvar::undefinedValue}}4}}{{setvar::later::no}}", &input);
        assert_eq!(result.result, Err(ReadError::HistoryAccess(cbs::units("row 4"))));
        assert_eq!(result.effects[0].value, Value::Undefined);
        assert_eq!(result.effects.len(), 1);
        let mut current = input.clone(); current.context.index = 4;
        assert_eq!(history_eval("{{role}}", &current).result, Err(ReadError::HistoryAccess(cbs::units("row 4"))));
        let skipped = history_eval("{{#if 0}}{{lastmessage}}{{setvar::x::1}}{{/if}}", &input);
        assert_eq!(skipped.result, Ok(Vec::new()));
        assert!(!skipped.history.unwrap().count);
        assert_eq!(history_eval("{{#when 0}}{{setvar::x::1}}{{lastmessage}}{{:else}}no{{/when}}", &input).effects.len(), 1);
    }

    #[test]
    fn history_output_is_inert_and_compiled_program_observes_each_capture() {
        use cbs::compat::{self, Cell, Outcome};
        let mut callbacks = compat::compile_with_history(&cbs::units("{{lastmessage}}{{lastmessageid}}{{previouschatlog::0}}{{role}}")).unwrap();
        assert_eq!(callbacks.fold_constants(), 0);
        let source = "{{settempvar::t::old}}{{lastmessage}}";
        let mut program = compat::compile_with_history(&cbs::units(source)).unwrap();
        program.fold_constants();
        let mut input = history_input(1, &[(0, "{{setvar::x::5}}{{gettempvar::t}}", "user")]);
        let Outcome::Evaluated(first) = compat::evaluate(&program, &input, &mut Budget::default()) else { panic!("first capture refused"); };
        assert_eq!(first.result, Ok(cbs::units("{{setvar::x::5}}{{gettempvar::t}}")));
        assert!(first.effects.is_empty());
        let second = history_eval(&String::from_utf16(&first.result.unwrap()).unwrap(), &input);
        assert_eq!(second.result, Ok(Vec::new()));
        assert_eq!(second.effects.len(), 1);
        for (key, value) in second.overlay { input.variables.insert(key, Cell::Value(value)); }
        input.history.as_mut().unwrap().rows[0].data = cbs::units("changed");
        input.history.as_mut().unwrap().provenance.capture_id = cbs::units("next");
        let Outcome::Evaluated(next) = compat::evaluate(&program, &input, &mut Budget::default()) else { panic!("next capture refused"); };
        assert_eq!(next.result, Ok(cbs::units("changed")));
        assert_eq!(next.history.unwrap().provenance.capture_id, cbs::units("next"));
        assert_eq!(history_eval("{{addvar::x::1}}{{getvar::x}}", &input).result, Ok(cbs::units("6")));
    }

    #[test]
    fn history_bounds_discard_private_prefixes_and_old_lanes_still_refuse() {
        use cbs::compat::{self, Outcome};
        let source = "{{setvar::x::1}}{{previouschatlog::0}}{{previouschatlog::1}}";
        let program = compat::compile_with_history(&cbs::units(source)).unwrap();
        let mut input = history_input(2, &[(0, "zero", "user"), (1, "one", "char")]);
        input.history.as_mut().unwrap().limits.dependencies = 1;
        assert_eq!(compat::evaluate(&program, &input, &mut Budget::default()), Outcome::Interrupted("history-dependency-limit".into()));
        input.history.as_mut().unwrap().limits.dependencies = 2;
        let success = history_eval(source, &input);
        assert_eq!(success.result, Ok(cbs::units("zeroone")));
        assert_eq!(success.effects.len(), 1);
        let large = "A".repeat(128);
        let input = history_input(1, &[(0, &large, "user")]);
        let program = compat::compile_with_history(&cbs::units("{{setvar::x::1}}{{lastmessage}}")).unwrap();
        assert_eq!(compat::evaluate(&program, &input, &mut Budget { max_output: 64, ..Budget::default() }), Outcome::Interrupted("output-limit".into()));
        assert_eq!(history_eval("{{setvar::x::1}}{{lastmessage}}", &input).result, Ok(cbs::units(&large)));
        let retained_row = "R".repeat(16_384);
        let mut input = history_input(1, &[(0, &retained_row, "user")]);
        input.history.as_mut().unwrap().limits.retained_bytes = 49_152;
        let admitted = history_eval("{{setvar::x::1}}", &input);
        assert_eq!(admitted.result, Ok(Vec::new()));
        assert_eq!(admitted.effects, vec![compat::Write { sequence: 0, key: effect_text("x"), value: effect_text("1") }]);
        assert!(!admitted.history.unwrap().count);
        assert_eq!(compat::evaluate(&program, &input, &mut Budget { max_output: 65_536, ..Budget::default() }),
            Outcome::Interrupted("history-retained-limit".into()));
        input.history.as_mut().unwrap().limits.retained_bytes = 131_072;
        let Outcome::Evaluated(success) = compat::evaluate(&program, &input, &mut Budget { max_output: 65_536, ..Budget::default() }) else { panic!("retained-byte success control refused"); };
        assert_eq!(success.result, Ok(cbs::units(&retained_row)));
        assert_eq!(success.effects, vec![compat::Write { sequence: 0, key: effect_text("x"), value: effect_text("1") }]);
        for source in ["{{lastmessage}}", "{{lastmessageid}}", "{{lastmessageindex}}", "{{previous_chat_log::0}}", "{{role}}"] {
            let pure = cbs::compile(&cbs::units(source)).unwrap();
            assert_eq!(evaluate(&pure, &mut Context::default(), &mut Budget::default()).unwrap_err(), "unsupported-operator");
            assert!(super::relaxed::prepare(&cbs::units(source), &Context::default()).is_err());
            assert_eq!(effect_eval_rejection(source), Outcome::Rejected("unsupported-operator".into()));
        }
        assert_eq!(compat::compile(&cbs::units("{{#if 0}}{{lastmessage}}{{/if}}")).unwrap_err(), "lazy-body-domain");
        for source in ["{{#if 0}}{{{{getvar::name}}}}{{/if}}", "{{#if 0}}{{previoususerchat}}{{/if}}"] {
            assert_eq!(compat::compile_with_history(&cbs::units(source)).unwrap_err(), "lazy-body-domain");
        }
    }
    fn effect_eval_rejection(source: &str) -> cbs::compat::Outcome {
        cbs::compat::evaluate(&cbs::compat::compile(&cbs::units(source)).unwrap(), &effect_input(&[]), &mut Budget::default())
    }

    #[test]
    fn history_admission_and_lookup_work_are_bounded() {
        use cbs::compat::{self, Outcome};
        let program = compat::compile_with_history(&cbs::units("{{setvar::x::1}}{{previouschatlog::1}}")).unwrap();
        for reason in ["duplicate", "outside", "unsafe-length", "unsafe-index", "label", "row-limit", "units", "bytes", "zero"] {
            let mut input = history_input(2, &[(0, "zero", "user"), (1, "one", "char")]);
            let history = input.history.as_mut().unwrap();
            match reason {
                "duplicate" => history.rows[1].index = 0,
                "outside" => history.rows[1].index = 2,
                "unsafe-length" => history.length = 9_007_199_254_740_992,
                "unsafe-index" => input.context.index = i64::MIN,
                "label" => history.provenance.capture_id = vec![0; 257],
                "row-limit" => history.limits.rows = 1,
                "units" => history.limits.input_units = 1,
                "bytes" => history.limits.retained_bytes = 1,
                _ => history.limits.dependencies = 0,
            }
            assert!(matches!(compat::evaluate(&program, &input, &mut Budget::default()), Outcome::Rejected(_)), "{reason}");
        }
        let rows: Vec<_> = (0..32).map(|index| (index, "x", "user")).collect();
        let input = history_input(32, &rows);
        let source = (0..32).map(|index| format!("{{{{previouschatlog::{index}}}}}")).collect::<String>();
        let program = compat::compile_with_history(&cbs::units(&source)).unwrap();
        assert_eq!(compat::evaluate(&program, &input, &mut Budget { remaining: 900, ..Budget::default() }), Outcome::Interrupted("budget".into()));
        assert_eq!(history_eval(&source, &input).result, Ok(cbs::units(&"x".repeat(32))));
        let mut budget = Budget::default(); budget.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
        assert_eq!(compat::evaluate(&program, &input, &mut budget), Outcome::Interrupted("cancelled".into()));
    }

    #[test]
    fn cancellation_reaches_history_row_and_dependency_scans() {
        use cbs::compat::{self, Outcome};
        let rows: Vec<_> = (0..1000).map(|index| (index, "x", "user")).collect();
        let input = history_input(1000, &rows);
        let source = (0..1000).map(|index| format!("{{{{previouschatlog::{index}}}}}")).collect::<String>();
        let program = compat::compile_with_history(&cbs::units(&source)).unwrap();
        let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let progress = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut budget = Budget { remaining: 10_000_000, cancel: cancel.clone(), progress: progress.clone(), ..Budget::default() };
        let worker = std::thread::spawn(move || compat::evaluate(&program, &input, &mut budget));
        while progress.load(std::sync::atomic::Ordering::Relaxed) < 600_000 {
            assert!(!worker.is_finished(), "history scans did not report bounded work");
            std::thread::yield_now();
        }
        cancel.store(true, std::sync::atomic::Ordering::Relaxed);
        assert_eq!(worker.join().unwrap(), Outcome::Interrupted("cancelled".into()));
    }

    #[test]
    fn effect_flags_preserve_nested_reads_and_remove_before_running() {
        for op in ["setvar", "addvar", "setdefaultvar"] {
            for run_var in [false, true] {
                for rm_var in [false, true] {
                    let mut input = effect_input(&[("value", "2")]);
                    input.run_var = run_var;
                    input.rm_var = rm_var;
                    let source = format!("{{{{{op}::score::{{{{getvar::value}}}}}}}}");
                    let result = effect_eval(&source, &input);
                    let preserved = format!("{{{{{op}::score::2}}}}");
                    assert_eq!(result.result, Ok(cbs::units(if run_var || rm_var { "" } else { &preserved })));
                    let active = run_var && !rm_var;
                    assert_eq!(result.effects.len(), usize::from(active));
                    assert_eq!(result.reads.iter().map(|r| r.key.clone()).collect::<Vec<_>>(),
                        if active && op != "setvar" { vec![effect_text("value"), effect_text("score")] }
                        else { vec![effect_text("value")] });
                }
            }
        }
    }

    #[test]
    fn effect_overlay_preserves_order_missing_arguments_and_numeric_defaults() {
        use cbs::compat::{Cell, Value};
        let input = effect_input(&[("score", "2")]);
        let result = effect_eval("{{setvar::score::4}}{{addvar::score::3}}{{getvar::score}}", &input);
        assert_eq!(result.result, Ok(cbs::units("7")));
        assert_eq!(result.effects.iter().map(|w| (w.sequence, w.value.clone())).collect::<Vec<_>>(),
            vec![(0, effect_text("4")), (2, effect_text("7"))]);
        assert_eq!(result.reads.iter().map(|r| r.sequence).collect::<Vec<_>>(), vec![1, 3]);
        assert_eq!(input.variables[&cbs::units("score")], Cell::Value(effect_text("2")));
        let result = effect_eval("{{setvar}}{{setvar::::}}{{setvar::x}}{{getvar::x}}{{setvar::x::undefined}}{{getvar::x}}", &input);
        assert_eq!(result.result, Ok(cbs::units("nullundefined")));
        assert_eq!(result.effects[0].key, Value::Undefined);
        assert_eq!(result.effects[0].value, Value::Undefined);
        assert_eq!(result.effects[1].key, effect_text(""));
        assert_eq!(result.effects[1].value, effect_text(""));
        assert_eq!(result.effects[2].value, Value::Undefined);
        assert_eq!(result.overlay, vec![(cbs::units("undefined"), Value::Undefined),
            (cbs::units(""), effect_text("")), (cbs::units("x"), effect_text("undefined"))]);
        for (initial, amount, expected) in [("2", "", "2"), ("0x10", "0b10", "18"),
            ("Infinity", "-Infinity", "NaN"), ("-0", "-0", "0"), ("bad", "2", "NaN")] {
            let input = effect_input(&[("x", initial)]);
            let result = effect_eval(&format!("{{{{addvar::x::{amount}}}}}{{{{getvar::x}}}}"), &input);
            assert_eq!(result.result, Ok(cbs::units(expected)));
        }
        assert_eq!(effect_eval("{{addvar::x}}{{getvar::x}}", &input).result, Ok(cbs::units("NaN")));
        for (initial, expected) in [("", "default"), ("null", "default"), ("0", "0"),
            ("false", "false"), ("undefined", "undefined")] {
            let result = effect_eval("{{setdefaultvar::x::default}}{{getvar::x}}", &effect_input(&[("x", initial)]));
            assert_eq!(result.result, Ok(cbs::units(expected)));
            assert_eq!(result.effects.len(), usize::from(expected == "default"));
        }
    }

    #[test]
    fn effect_snapshot_and_conditionals_follow_their_distinct_paths() {
        use cbs::compat::{Binding, Outcome};
        let mut input = effect_input(&[("x", "2")]);
        input.binding = Binding::SnapshotOnly;
        let result = effect_eval("{{setvar::x::9}}{{addvar::x::3}}{{setdefaultvar::missing::v}}{{getvar::x}}", &input);
        assert_eq!(result.result, Ok(cbs::units("2")));
        assert!(result.effects.is_empty() && result.overlay.is_empty());
        assert_eq!(result.reads.iter().map(|r| r.key.clone()).collect::<Vec<_>>(),
            vec![effect_text("x"), effect_text("missing"), effect_text("x")]);
        input.binding = Binding::MutableMap;
        let result = effect_eval("{{#if 0}}{{setvar::x::9}}{{getvar::absent}}{{/if}}{{#if_pure 1}}{{setvar::x::3}}{{/if}}{{#when 0}}{{setvar::x::4}}yes{{:else}}{{addvar::x::2}}no{{/when}}{{getvar::x}}", &input);
        assert_eq!(result.result, Ok(cbs::units("no6")));
        assert_eq!(result.effects.iter().map(|w| w.value.clone()).collect::<Vec<_>>(),
            vec![effect_text("3"), effect_text("4"), effect_text("6")]);
        assert!(!result.reads.iter().any(|r| r.key == effect_text("absent")));
        for source in ["{{setvar::x::1}}{{#if 0}}{{call::f}}{{/if}}",
            "{{#if 0}}{{{{getvar::name}}::x::1}}{{/if}}"] {
            assert_eq!(cbs::compat::compile(&cbs::units(source)).unwrap_err(), "lazy-body-domain");
        }
        let p = cbs::compile(&cbs::units("{{setvar::x::1}}")).unwrap();
        assert_eq!(evaluate(&p, &mut Context::default(), &mut Budget::default()).unwrap_err(), "unsupported-operator");
        assert!(super::relaxed::prepare(&cbs::units("{{setvar::x::1}}"), &Context::default()).is_err());
        let p = cbs::compat::compile(&cbs::units("{{setvar::x::1}}{{unsupported}}")).unwrap();
        assert_eq!(cbs::compat::evaluate(&p, &input, &mut Budget::default()), Outcome::Rejected("unsupported-operator".into()));
    }

    #[test]
    fn effect_read_errors_preserve_callback_and_condition_boundaries() {
        use cbs::compat::{Cell, ReadError};
        for error in [ReadError::Ordinary(cbs::units("synthetic fault")), ReadError::HistoryAccess(cbs::units("row:2"))] {
            let mut input = effect_input(&[]);
            input.variables.insert(cbs::units("fault"), Cell::Error(error.clone()));
            let direct = effect_eval("{{setvar::a::1}}{{getvar::fault}}{{setvar::b::2}}", &input);
            if matches!(error, ReadError::Ordinary(_)) {
                assert_eq!(direct.result, Ok(cbs::units("{{getvar::fault}}")));
                assert_eq!(direct.effects.iter().map(|w| w.key.clone()).collect::<Vec<_>>(), vec![effect_text("a"), effect_text("b")]);
            } else {
                assert_eq!(direct.result, Err(error.clone()));
                assert_eq!(direct.effects.len(), 1);
                let nested = effect_eval("{{setvar::a::1}}{{setvar::b::{{getvar::fault}}}}", &input);
                assert_eq!(nested.result, Err(error.clone()));
                assert_eq!(nested.effects.len(), 1);
            }
            let conditional = effect_eval("{{setvar::a::1}}{{#when::var::fault}}{{setvar::body::3}}{{/when}}{{setvar::b::2}}", &input);
            assert_eq!(conditional.result, Err(error));
            assert_eq!(conditional.effects.len(), 1);
            assert_eq!(conditional.reads, vec![cbs::compat::Read { sequence: 1, key: effect_text("fault") }]);
            assert_eq!(effect_eval("{{setvar::fault::1}}{{#when::var::fault}}yes{{/when}}", &input).result, Ok(cbs::units("yes")));
        }
        let mut input = effect_input(&[]);
        input.variables.insert(cbs::units("fault"), Cell::Error(ReadError::Ordinary(cbs::units("ordinary"))));
        let nested = effect_eval("{{#when {{getvar::fault}}}}{{setvar::x::yes}}{{/when}}{{getvar::x}}", &input);
        assert_eq!(nested.result, Ok(cbs::units("yes")));
        assert_eq!(nested.effects.len(), 1);
    }

    #[test]
    fn effect_programs_remain_dynamic_and_generated_text_waits_for_a_checkpoint() {
        use cbs::compat::{Cell, Outcome};
        let mut program = cbs::compat::compile(&cbs::units("{{setvar::x::{{getvar::source}}}}{{addvar::x::1}}{{getvar::x}}")).unwrap();
        program.fold_constants();
        for (source, expected) in [("2", "3"), ("8", "9")] {
            let input = effect_input(&[("source", source)]);
            let Outcome::Evaluated(result) = cbs::compat::evaluate(&program, &input, &mut Budget::default()) else { panic!("refused"); };
            assert_eq!(result.result, Ok(cbs::units(expected)));
            assert_eq!(result.effects.len(), 2);
        }
        let mut input = effect_input(&[("name", "setvar"), ("tail", "::x::7")]);
        assert_eq!(effect_eval("{{{{getvar::name}}{{getvar::tail}}}}{{getvar::x}}", &input).result, Ok(cbs::units("7")));
        input.variables.insert(cbs::units("next"), Cell::Value(effect_text("{{setvar::x::5}}{{gettempvar::t}}")));
        let first = effect_eval("{{settempvar::t::old}}{{getvar::next}}", &input);
        assert!(first.effects.is_empty());
        let source = String::from_utf16(&first.result.unwrap()).unwrap();
        let second = effect_eval(&source, &input);
        assert_eq!(second.result, Ok(Vec::new()));
        assert_eq!(second.effects.len(), 1);
        for (key, value) in second.overlay { input.variables.insert(key, Cell::Value(value)); }
        assert_eq!(effect_eval("{{addvar::x::1}}{{getvar::x}}", &input).result, Ok(cbs::units("6")));
    }

    #[test]
    fn effect_rejection_and_interruption_never_expose_a_speculative_prefix() {
        use cbs::compat::{self, Binding, Cell, Outcome, ReadError};
        let source = "{{setvar::x::1}}{{getvar::x}}{{setvar::y::2}}{{getvar::y}}";
        let program = compat::compile(&cbs::units(source)).unwrap();
        for reason in ["effect-limit", "read-limit", "retained-limit", "budget", "cancelled"] {
            let mut input = effect_input(&[]);
            let mut budget = Budget::default();
            match reason {
                "effect-limit" => input.limits.effects = 1,
                "read-limit" => input.limits.reads = 1,
                "retained-limit" => input.limits.retained_bytes = 1,
                "budget" => budget.remaining = 4,
                "cancelled" => budget.cancel.store(true, std::sync::atomic::Ordering::Relaxed),
                _ => unreachable!(),
            }
            assert_eq!(compat::evaluate(&program, &input, &mut budget), Outcome::Interrupted(reason.into()));
            assert!(input.variables.is_empty());
        }
        let large = "A".repeat(128);
        let input = effect_input(&[("large", &large)]);
        let after_write = compat::compile(&cbs::units("{{setvar::x::1}}{{getvar::large}}")).unwrap();
        let mut budget = Budget { max_output: 64, ..Budget::default() };
        assert_eq!(compat::evaluate(&after_write, &input, &mut budget), Outcome::Interrupted("output-limit".into()));
        let mut budget = Budget { max_output: 256, ..Budget::default() };
        let Outcome::Evaluated(success) = compat::evaluate(&after_write, &input, &mut budget) else { panic!("success control refused"); };
        assert_eq!(success.result, Ok(cbs::units(&large)));
        assert_eq!(success.effects.len(), 1);
        assert_eq!(success.effects[0].key, effect_text("x"));
        assert_eq!(success.effects[0].value, effect_text("1"));
        let input = effect_input(&[("name", "unknown")]);
        let rejected = compat::compile(&cbs::units("{{setvar::x::1}}{{{{getvar::name}}}}")).unwrap();
        for _ in 0..2 {
            assert_eq!(compat::evaluate(&rejected, &input, &mut Budget::default()), Outcome::Rejected("unsupported-operator".into()));
            assert!(!input.variables.contains_key(&cbs::units("x")));
        }
        let mut snapshot = effect_input(&[]);
        snapshot.binding = Binding::SnapshotOnly;
        snapshot.variables.insert(cbs::units("fault"), Cell::Error(ReadError::Ordinary(cbs::units("fault"))));
        assert_eq!(compat::evaluate(&program, &snapshot, &mut Budget::default()), Outcome::Rejected("binding-domain".into()));
    }

    #[test]
    fn effect_search_work_is_bounded_for_reads_writes_and_conditions() {
        use cbs::compat::{self, Outcome};
        let keys: Vec<_> = (0..32).map(|index| format!("k{index:02}")).collect();
        let entries: Vec<_> = keys.iter().map(|key| (key.as_str(), "1")).collect();
        let input = effect_input(&entries);
        for kind in ["reads", "writes", "conditions"] {
            let source = keys.iter().map(|key| match kind {
                "reads" => format!("{{{{getvar::{key}}}}}"),
                "writes" => format!("{{{{setvar::{key}::1}}}}"),
                _ => format!("{{{{#when::var::{key}}}}}Y{{{{/when}}}}"),
            }).collect::<String>();
            let program = compat::compile(&cbs::units(&source)).unwrap();
            let mut small = Budget { remaining: 600, ..Budget::default() };
            assert_eq!(compat::evaluate(&program, &input, &mut small), Outcome::Interrupted("budget".into()), "{kind}");
            let mut enough = Budget { remaining: 10_000, ..Budget::default() };
            let Outcome::Evaluated(result) = compat::evaluate(&program, &input, &mut enough) else { panic!("{kind} success control refused"); };
            let expected = match kind { "reads" => "1".repeat(32), "writes" => String::new(), _ => "Y".repeat(32) };
            assert_eq!(result.result, Ok(cbs::units(&expected)));
            assert_eq!(result.effects.len(), if kind == "writes" { 32 } else { 0 });
        }
    }

    #[test]
    fn cancellation_reaches_effect_dependency_scans() {
        use cbs::compat::{self, Outcome};
        let keys: Vec<_> = (0..1000).map(|index| format!("{}{index:04}", "k".repeat(512))).collect();
        let entries: Vec<_> = keys.iter().map(|key| (key.as_str(), "1")).collect();
        let input = effect_input(&entries);
        let source = keys.iter().map(|key| format!("{{{{getvar::{key}}}}}")).collect::<String>();
        let program = compat::compile(&cbs::units(&source)).unwrap();
        let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let progress = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut budget = Budget { remaining: 100_000_000, cancel: cancel.clone(), progress: progress.clone(), ..Budget::default() };
        let worker = std::thread::spawn(move || compat::evaluate(&program, &input, &mut budget));
        while progress.load(std::sync::atomic::Ordering::Relaxed) < 20_000 {
            assert!(!worker.is_finished(), "dependency comparisons did not report bounded work");
            std::thread::yield_now();
        }
        cancel.store(true, std::sync::atomic::Ordering::Relaxed);
        assert_eq!(worker.join().unwrap(), Outcome::Interrupted("cancelled".into()));
    }

    fn program_input(entries: &[(&str, &str)]) -> cbs::compat::Input {
        let mut input = effect_input(entries);
        input.program = Some(cbs::program::Limits::default());
        input
    }
    fn program_eval(source: &str, input: &cbs::compat::Input) -> cbs::compat::Evaluation {
        let mut program = cbs::compat::compile_program(&cbs::units(source), input.history.is_some()).unwrap();
        assert_eq!(program.fold_constants(), 0, "invocation state must not fold");
        match cbs::compat::evaluate(&program, input, &mut cbs::Budget::default()) {
            cbs::compat::Outcome::Evaluated(value) => value,
            other => panic!("program refused: {other:?}"),
        }
    }

    #[test]
    fn program_calls_execute_before_preservation_and_capture_their_output() {
        let input = program_input(&[("x", "0")]);
        for (source, expected) in [
            ("{{#func f}}{{#when 1}}\n  A\n{{:else}}\n B\n{{/when}}{{/func}}{{call::f}}", "  A"),
            ("{{#func f}}{{#when::keep::0}}\n  A\n{{:else}}\n B\n{{/when}}{{/func}}{{call::f}}", " B\n"),
            ("{{#func f}}{{#if 1}}\n  A\n B  \n{{/if}}{{/func}}{{call::f}}", "A\nB"),
            ("{{#func f}}{{#if_pure 1}}\n A\n{{/if}}{{/func}}{{call::f}}", "\n A\n"),
        ] { assert_eq!(program_eval(source, &input).result, Ok(cbs::units(expected))); }

        let value = program_eval("{{#func bump}}{{addvar::x::1}}{{/func}}{{#if 0}}{{call::bump}}{{/if}}{{getvar::x}}", &input);
        assert_eq!(value.result, Ok(cbs::units("1")));
        assert_eq!(value.effects.len(), 1);
        let value = program_eval("{{#func emit}}{{addvar::x::1}}B{{/func}}{{#func f}}A{{call::emit}}C{{/func}}{{getvar::x}}/{{call::f}}/{{getvar::x}}", &input);
        assert_eq!(value.result, Ok(cbs::units("1/ABC/1")));
        assert_eq!(value.effects.len(), 1);
        assert_eq!(program_eval("{{#func f}}{{#func g}}G{{/func}}{{call::g}}{{/func}}{{call::f}}/{{call::g}}", &input).result, Ok(cbs::units("G/G")));
        assert_eq!(program_eval("{{#func  ignored}} E {{/func}}[{{call::}}]", &input).result, Ok(cbs::units("[E]")));
    }

    #[test]
    fn program_substitution_uses_argument_zero_and_sequential_js_tokens() {
        let input = program_input(&[]);
        for (body, args, expected) in [
            ("{{arg::0}}/{{arg::1}}", "::X", "f/X"),
            ("<{{arg::1}}>", "::$&", "<{{arg::1}}>"),
            ("A{{arg::1}}B", "::$$", "A$B"),
            ("A{{arg::1}}B", "::$\u{0060}", "AAB"),
            ("A{{arg::1}}B", "::$'", "ABB"),
            ("{{arg::1}}{{arg::2}}", "::$'::Z", "ZZ"),
            ("{{arg::1}}", "::$1$99$<name>", "$1$99$<name>"),
        ] {
            let source = format!("{{{{#func f}}}}{body}{{{{/func}}}}{{{{call::f{args}}}}}");
            assert_eq!(program_eval(&source, &input).result, Ok(cbs::units(expected)), "{source}");
        }
        let input = program_input(&[("name", "f"), ("value", "X")]);
        assert_eq!(program_eval("{{#func f}}{{arg::1}}{{/func}}{{call::{{getvar::name}}::{{getvar::value}}}}", &input).result, Ok(cbs::units("X")));
    }

    #[test]
    fn program_residual_arguments_preserve_spelling_without_a_second_substitution() {
        let input = program_input(&[("index", "1")]);
        assert_eq!(program_eval("{{#func f}}{{arg::1}}{{/func}}{{call::f}}", &input).result, Ok(cbs::units("{{arg::1}}")));
        assert_eq!(program_eval("{{#func f}}{{arg::1}}{{/func}}{{call::f::}}", &input).result, Ok(Vec::new()));
        for token in ["{{ARG::1}}", "{{ arg::1}}", "{{a_r-g:1}}", "{{arg:1}}", "{{arg}}", "{{arg::}}",
            "{{arg::01}}", "{{arg::-0}}", "{{arg::-1}}", "{{arg::NaN}}", "{{arg::x}}", "{{arg::1::extra}}"] {
            let source = format!("{{{{#func f}}}}{token}{{{{/func}}}}{{{{call::f::X}}}}");
            assert_eq!(program_eval(&source, &input).result, Ok(cbs::units(token)), "{token}");
        }
        assert_eq!(program_eval("{{#func f}}{{arg::{{getvar::index}}}}{{/func}}{{call::f::X}}", &input).result, Ok(cbs::units("{{arg::1}}")));
        let value = program_eval("{{arg::{{setvar::x::1}}}}", &input);
        assert_eq!(value.result, Ok(cbs::units("{{arg::}}")));
        assert_eq!(value.effects.len(), 1);
        assert_eq!(program_eval("{{arg::{{return::done}}}}after", &input).result, Ok(cbs::units("done")));
        assert_eq!(program_eval("{{#if 0}}{{arg::{{setvar::x::1}}}}{{/if}}", &input).effects.len(), 0);
    }

    #[test]
    fn program_children_isolate_temps_and_share_function_redefinitions() {
        let input = program_input(&[]);
        let value = program_eval("{{#func f}}[{{tempvar::t}}]{{settempvar::t::child}}{{tempvar::t}}{{/func}}{{settempvar::t::parent}}{{call::f}}/{{tempvar::t}}", &input);
        assert_eq!(value.result, Ok(cbs::units("[]child/parent")));
        let source = "{{#func g}}old{{/func}}{{#func f}}{{#func g}}new{{/func}}{{/func}}{{call::f}}{{call::g}}";
        assert_eq!(program_eval(source, &input).result, Ok(cbs::units("new")));
        assert_eq!(program_eval("{{call::g}}", &input).result, Ok(cbs::units("{{call::g}}")));
    }

    #[test]
    fn program_sibling_calls_consume_the_shared_counter_including_literal_children() {
        let input = program_input(&[]);
        let source = format!("{{{{#func f}}}}R{{{{/func}}}}{}{{{{call::unknown}}}}", "{{call::f}}".repeat(20));
        let expected = format!("{}{}", "R".repeat(19), "ERROR: Call stack limit reached".repeat(2));
        assert_eq!(program_eval(&source, &input).result, Ok(cbs::units(&expected)));
        let source = "{{#func f}}{{call::f}}{{/func}}{{call::f}}";
        assert_eq!(program_eval(source, &input).result, Ok(cbs::units("ERROR: Call stack limit reached")));
    }

    #[test]
    fn program_return_stops_the_current_frame_and_returned_cbs_waits_for_a_checkpoint() {
        let input = program_input(&[]);
        for (source, expected) in [("before{{return}}after", "null"), ("before{{return::}}after", ""),
            ("before{{return::undefined}}after", "undefined"), ("{{equal::{{return::R}}::x}}after", "R"),
            ("{{#func f}}before{{return::R}}after{{/func}}A{{call::f}}B", "ARB")] {
            assert_eq!(program_eval(source, &input).result, Ok(cbs::units(expected)), "{source}");
        }
        let value = program_eval("before{{setvar::x::1}}{{return::R}}{{setvar::y::2}}after", &input);
        assert_eq!(value.result, Ok(cbs::units("R")));
        assert_eq!(value.effects.len(), 1);
        assert_eq!(value.effects[0].key, effect_text("x"));
        let input = program_input(&[("raw", "{{setvar}}")]);
        let first = program_eval("{{#func f}}{{return::{{getvar::raw}}}}{{/func}}A{{call::f}}B", &input);
        assert_eq!(first.result, Ok(cbs::units("A{{setvar}}B")));
        assert!(first.effects.is_empty());
        let next = program_eval("A{{setvar}}B", &input);
        assert_eq!(next.result, Ok(cbs::units("AB")));
        assert_eq!(next.effects[0].key, cbs::compat::Value::Undefined);
        assert_eq!(next.effects[0].value, cbs::compat::Value::Undefined);
    }

    #[test]
    fn program_child_errors_retain_parent_and_child_effect_prefixes() {
        let mut input = history_input(10, &[(3, "three", "user")]);
        input.program = Some(cbs::program::Limits::default());
        let source = "{{#func f}}{{setvar::child::1}}{{previouschatlog::3}}{{lastmessage}}{{setvar::late::1}}{{/func}}{{setvar::parent::1}}{{call::f}}{{setvar::suffix::1}}";
        let value = program_eval(source, &input);
        assert_eq!(value.result, Err(cbs::compat::ReadError::HistoryAccess(cbs::units("row 9"))));
        assert_eq!(value.effects.iter().map(|e| e.key.clone()).collect::<Vec<_>>(), vec![effect_text("parent"), effect_text("child")]);
        assert_eq!(value.history.unwrap().rows, vec![3, 9]);
        let mut input = program_input(&[]);
        input.variables.insert(cbs::units("fault"), cbs::compat::Cell::Error(cbs::compat::ReadError::Ordinary(cbs::units("failure"))));
        let value = program_eval("{{#func f}}{{setvar::x::1}}{{#when::var::fault}}body{{/when}}{{/func}}{{call::f}}suffix", &input);
        assert_eq!(value.result, Err(cbs::compat::ReadError::Ordinary(cbs::units("failure"))));
        assert_eq!(value.effects.len(), 1);
        assert_eq!(program_eval("{{#func f}}{{getvar::fault}}{{/func}}{{call::f}}suffix", &input).result, Ok(cbs::units("{{getvar::fault}}suffix")));
    }

    #[test]
    fn program_refusals_and_resource_interruptions_never_expose_a_prefix() {
        use cbs::compat::{self, Outcome};
        let input = program_input(&[]);
        for source in ["{{setvar::x::1}}{{unknown}}", "{{arg::{{unknown}}}}",
            "{{#each x as y}}x{{/each}}", "{{#func::f}}x{{/func}}",
            "{{{{getvar::kind}}}}x{{/func}}", "{{#func f}}x{{/{{user}}}}"] {
            let program = compat::compile_program(&cbs::units(source), false).unwrap();
            assert!(matches!(compat::evaluate(&program, &input, &mut cbs::Budget::default()), Outcome::Rejected(_)), "{source}");
        }
        assert!(cbs::compile(&cbs::units("{{#func f}}x{{/func}}")).is_err());
        assert!(compat::compile(&cbs::units("{{#func f}}x{{/func}}")).is_err());
        let program = compat::compile_program(&cbs::units("plain"), false).unwrap();
        assert_eq!(compat::evaluate(&program, &effect_input(&[]), &mut cbs::Budget::default()), Outcome::Rejected("program-domain".into()));
        let ordinary = compat::compile(&cbs::units("plain")).unwrap();
        assert_eq!(compat::evaluate(&ordinary, &input, &mut cbs::Budget::default()), Outcome::Rejected("program-domain".into()));
        for (kind, source, reason) in [
            ("functions", "{{#func f}}F{{/func}}{{setvar::x::1}}{{#func g}}G{{/func}}", "program-function-limit"),
            ("frames", "{{#func f}}F{{/func}}{{setvar::x::1}}{{call::f}}", "program-frame-limit"),
            ("expanded", "{{#func f}}hello{{/func}}{{setvar::x::1}}{{call::f}}", "program-expanded-limit"),
        ] {
            let mut low = input.clone();
            let limits = low.program.as_mut().unwrap();
            match kind { "functions" => limits.functions = 1, "frames" => limits.frames = 1, _ => limits.expanded_units = 1 }
            let program = compat::compile_program(&cbs::units(source), false).unwrap();
            assert_eq!(compat::evaluate(&program, &low, &mut cbs::Budget::default()), Outcome::Interrupted(reason.into()));
            assert_eq!(program_eval(source, &input).effects.len(), 1);
        }
        let expanded_source = "{{#func f}}".to_owned() + &"a".repeat(64) + "{{arg::1}}" + &"b".repeat(64)
            + "{{/func}}{{setvar::x::1}}{{call::f::$\u{0060}$\u{0060}$\u{0060}}}";
        let expanded_program = compat::compile_program(&cbs::units(&expanded_source), false).unwrap();
        let mut short_source = input.clone(); short_source.program.as_mut().unwrap().source_units = 256;
        assert_eq!(compat::evaluate(&expanded_program, &short_source, &mut cbs::Budget::default()),
            Outcome::Interrupted("program-source-limit".into()));
        assert_eq!(program_eval(&expanded_source, &input).effects.len(), 1);

        let mut zero = input.clone(); zero.program.as_mut().unwrap().frames = 0;
        assert_eq!(compat::evaluate(&program, &zero, &mut cbs::Budget::default()), Outcome::Rejected("invalid-program-limits".into()));
        let mut small = input.clone(); small.program.as_mut().unwrap().source_units = 1;
        assert_eq!(compat::evaluate(&program, &small, &mut cbs::Budget::default()), Outcome::Rejected("program-input-limit".into()));
    }

    #[test]
    fn program_retained_output_and_work_bounds_discard_computed_effects() {
        use cbs::compat::{self, Outcome};
        let large = "A".repeat(4096);
        let mut input = program_input(&[("large", &large)]);
        let source = "{{setvar::x::1}}{{getvar::large}}";
        let program = compat::compile_program(&cbs::units(source), false).unwrap();
        input.program.as_mut().unwrap().retained_bytes = 2048;
        let writer = compat::compile_program(&cbs::units("{{setvar::x::1}}"), false).unwrap();
        assert!(matches!(compat::evaluate(&writer, &input, &mut cbs::Budget::default()), Outcome::Evaluated(_)));
        assert_eq!(compat::evaluate(&program, &input, &mut cbs::Budget::default()), Outcome::Interrupted("program-retained-limit".into()));
        input.program.as_mut().unwrap().retained_bytes = 65536;
        assert_eq!(program_eval(source, &input).effects.len(), 1);
        let mut budget = cbs::Budget { max_output: 64, ..cbs::Budget::default() };
        assert_eq!(compat::evaluate(&program, &input, &mut budget), Outcome::Interrupted("output-limit".into()));
        {
            let parent = "P".repeat(4096);
            let child = "c".repeat(256);
            let body = (0..100).map(|i| format!("{{{{settempvar::c{i:03}::{{{{getvar::child}}}}}}}}")).collect::<String>();
            let prefix = format!("{{{{#func f}}}}[{{{{gettempvar::p}}}}]{body}{{{{settempvar::p::child}}}}{{{{gettempvar::p}}}}{{{{/func}}}}{{{{setvar::effect::kept}}}}");
            for (stage, parent_live, child_live, bytes, expected) in [
                ("parent-only", true, false, 86_016, Some(format!("/{parent}"))),
                ("child-only", false, true, 86_016, Some("[]child/".to_owned())),
                ("combined-low", true, true, 86_016, None),
                ("combined-high", true, true, 262_144, Some(format!("[]child/{parent}"))),
            ] {
                let source = format!("{prefix}{}{}/{{{{gettempvar::p}}}}",
                    if parent_live { "{{settempvar::p::{{getvar::parent}}}}" } else { "" },
                    if child_live { "{{call::f}}" } else { "" });
                let mut input = program_input(&[("parent", &parent), ("child", &child)]);
                input.program.as_mut().unwrap().retained_bytes = bytes;
                let program = compat::compile_program(&cbs::units(&source), false).unwrap();
                let mut budget = cbs::Budget { max_output: 65_536, ..cbs::Budget::default() };
                let outcome = compat::evaluate(&program, &input, &mut budget);
                if let Some(expected) = expected {
                    let Outcome::Evaluated(value) = outcome else { panic!("{stage}: {outcome:?}"); };
                    assert_eq!(value.result, Ok(cbs::units(&expected)), "{stage}");
                    assert_eq!(value.effects, vec![compat::Write { sequence: 0, key: effect_text("effect"), value: effect_text("kept") }], "{stage}");
                } else {
                    assert_eq!(outcome, Outcome::Interrupted("program-retained-limit".into()), "{stage}");
                }
            }
        }

        let source = format!("{{{{#func f}}}}{}{{{{arg::1}}}}{{{{/func}}}}{{{{setvar::x::1}}}}{{{{call::f::X}}}}", "a".repeat(4096));
        let program = compat::compile_program(&cbs::units(&source), false).unwrap();
        let mut budget = cbs::Budget { remaining: 100, ..cbs::Budget::default() };
        assert_eq!(compat::evaluate(&program, &input, &mut budget), Outcome::Interrupted("budget".into()));
        assert_eq!(program_eval(&source, &input).effects.len(), 1);
        let mut budget = cbs::Budget::default();
        budget.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
        assert_eq!(compat::evaluate(&program, &input, &mut budget), Outcome::Interrupted("cancelled".into()));
    }

    #[test]
    fn cancellation_reaches_program_function_lookup_after_progress() {
        use cbs::compat::{self, Outcome};
        let source = (0..500).map(|i| format!("{{{{#func {}{i:04}}}}}F{{{{/func}}}}", "k".repeat(512))).collect::<String>();
        let mut input = program_input(&[]);
        input.program.as_mut().unwrap().functions = 512;
        let program = compat::compile_program(&cbs::units(&source), false).unwrap();
        let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let progress = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut budget = Budget { remaining: 100_000_000, cancel: cancel.clone(), progress: progress.clone(), ..Budget::default() };
        let worker = std::thread::spawn(move || compat::evaluate(&program, &input, &mut budget));
        while progress.load(std::sync::atomic::Ordering::Relaxed) < 100_000 {
            assert!(!worker.is_finished(), "function comparisons did not report bounded work");
            std::thread::yield_now();
        }
        cancel.store(true, std::sync::atomic::Ordering::Relaxed);
        assert_eq!(worker.join().unwrap(), Outcome::Interrupted("cancelled".into()));
    }
}
