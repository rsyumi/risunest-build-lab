//! Bounds the oracle cannot reach: nesting and recursion that would overflow the stack, budget limits, cancellation,
//! and the host-supplied clock, month names and case tables.

use risunest_ir::budget::{Budget, Error, Limits};
use risunest_ir::stage::StageError;
use risunest_ir::text::Text;
use risunest_ir::value::Value;
use risunest_jinja::{CaseTables, JinjaError, LocalTime, MonthNames, RenderOptions, Template};
use std::sync::{atomic::{AtomicBool, Ordering}, Arc};

const LIMITS: Limits = Limits {
    max_owned_bytes: 1 << 30,
    max_json_bytes: 1 << 24,
    max_text_units: 1 << 24,
    max_values: 1 << 22,
    max_depth: 128,
    max_work: 1 << 30,
};

fn budget(limits: Limits) -> Budget { Budget::new(limits, Arc::new(AtomicBool::new(false))).unwrap() }

fn render_with(source: &str, limits: Limits, options: &RenderOptions) -> Result<String, JinjaError> {
    let budget = budget(limits);
    let template = Template::new(&Text::from_str(source, &budget).unwrap(), &budget)?;
    let text = template.render(&Value::Undefined, options, &budget)?;
    Ok(String::from_utf16_lossy(&text.units().collect::<Vec<_>>()))
}

fn render(source: &str) -> Result<String, JinjaError> { render_with(source, LIMITS, &RenderOptions::default()) }

/// Runs `body` on a thread with the 8 MiB stack the engine assumes of a stage thread, so a recursion the limits miss
/// aborts the test process.
fn on_stage_stack<T: Send + 'static>(body: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new().stack_size(8 << 20).spawn(body).unwrap().join().unwrap()
}

fn limit(result: Result<String, JinjaError>) -> &'static str {
    match result {
        Err(JinjaError::Budget(Error::Limit(name))) => name,
        other => panic!("expected a limit, got {other:?}"),
    }
}

fn declined(result: Result<String, JinjaError>) -> &'static str {
    match result {
        Err(JinjaError::Unsupported(name)) => name,
        other => panic!("expected a decline, got {other:?}"),
    }
}

#[test]
fn parse_nesting_stops_at_the_depth_limit() {
    on_stage_stack(|| {
        let deep = 20_000;
        let parens = format!("{{{{ {}1{} }}}}", "(".repeat(deep), ")".repeat(deep));
        assert_eq!(declined(render(&parens)), "jinja-depth");
        let arrays = format!("{{{{ {}{} }}}}", "[".repeat(deep), "]".repeat(deep));
        assert_eq!(declined(render(&arrays)), "jinja-depth");
        let negations = format!("{{{{ {}1 }}}}", "not ".repeat(deep));
        assert_eq!(declined(render(&negations)), "jinja-depth");
        let blocks = format!("{}x{}", "{% if true %}".repeat(deep), "{% endif %}".repeat(deep));
        assert_eq!(declined(render(&blocks)), "jinja-depth");
        let filters = format!("{{{{ 1{} }}}}", "|string".repeat(deep));
        assert_eq!(declined(render(&filters)), "jinja-depth");
        // Nesting inside the limit still parses and renders.
        assert_eq!(render(&format!("{{{{ {}1{} }}}}", "(".repeat(100), ")".repeat(100))).unwrap(), "1");
        assert_eq!(render(&format!("{}x{}", "{% if true %}".repeat(100), "{% endif %}".repeat(100))).unwrap(), "x");
    });
}

#[test]
fn recursion_stops_at_the_depth_limit() {
    on_stage_stack(|| {
        assert_eq!(declined(render("{% macro f(n) %}{{ f(n + 1) }}{% endmacro %}{{ f(0) }}")), "jinja-depth");
        assert_eq!(declined(render("{% macro f() %}{% for x in [1] %}{% if true %}{{ f()|upper }}{% endif %}{% endfor %}{% endmacro %}{{ f() }}")), "jinja-depth");
        assert_eq!(declined(render("{% macro m() %}{% call m() %}x{% endcall %}{% endmacro %}{{ m() }}")), "jinja-depth");
        // The deepest recursion turning the deepest value into text.
        let walk = "{% set ns = namespace() %}{% set ns.me = ns %}{% macro f(n) %}{% if n > 0 %}{{ f(n - 1) }}{% else %}{{ ns }}{% endif %}{% endmacro %}";
        assert_eq!(declined(render(&format!("{walk}{{{{ f(250) }}}}"))), "jinja-depth");
        assert_eq!(declined(render(&format!("{walk}{{{{ f(100) }}}}"))), "jinja-depth");
        // Recursion inside the limit renders.
        assert_eq!(render("{% macro f(n) %}{% if n > 0 %}{{ f(n - 1) }}{% else %}done{% endif %}{% endmacro %}{{ f(20) }}").unwrap(), "done");
    });
}

#[test]
fn deep_values_are_released_without_recursion() {
    on_stage_stack(|| {
        // A `set` inside the loop would bind in the loop's scope, so the values grow inside a namespace.
        let nest = "{% set ns = namespace(a=[], o={}) %}{% for i in range(200000) %}{% set ns.a = [ns.a] %}{% set ns.o = {'k': ns.o} %}{% endfor %}";
        assert_eq!(render(&format!("{nest}{{{{ ns.a|length }}}}{{{{ ns.o|length }}}}{{{{ ns.a[0][0][0]|length }}}}")).unwrap(), "111");
        // Rendering one walks it, which stops at the depth limit as JS stops when its stack runs out.
        assert_eq!(declined(render(&format!("{nest}{{{{ ns.a }}}}"))), "jinja-depth");
        assert_eq!(declined(render(&format!("{nest}{{{{ ns.o|tojson }}}}"))), "jinja-depth");
        assert_eq!(declined(render(&format!("{nest}{{{{ ns.a == 'x' }}}}"))), "jinja-depth");
        let namespaces = "{% set h = namespace(n=namespace(v=none)) %}{% for i in range(200000) %}{% set h.n = namespace(v=h.n) %}{% endfor %}{{ h.n.v.v is defined }}";
        assert_eq!(render(namespaces).unwrap(), "true");
    });
}

#[test]
fn a_namespace_that_holds_itself_renders_and_stops_when_walked() {
    on_stage_stack(|| {
        assert_eq!(render("{% set ns = namespace() %}{% set ns.me = ns %}{{ ns.me.me.me is defined }}").unwrap(), "true");
        assert_eq!(declined(render("{% set ns = namespace() %}{% set ns.me = ns %}{{ ns }}")), "jinja-depth");
        assert_eq!(declined(render("{% set ns = namespace() %}{% set ns.l = [ns] %}{{ ns|tojson }}")), "jinja-depth");
    });
}

#[test]
fn work_and_output_are_budgeted() {
    let work = Limits { max_work: 100_000, ..LIMITS };
    assert_eq!(limit(render_with("{% for i in range(1000000) %}x{% endfor %}", work, &RenderOptions::default())), "work");
    assert_eq!(limit(render_with("{{ range(1000000)|length }}", work, &RenderOptions::default())), "work");
    let text = Limits { max_text_units: 1000, ..LIMITS };
    assert_eq!(limit(render_with("{% for i in range(2000) %}x{% endfor %}", text, &RenderOptions::default())), "text-units");
    assert_eq!(limit(render_with("{% set s = 'ab' %}{% for i in range(20) %}{% set s = s + s %}{% endfor %}", text, &RenderOptions::default())), "text-units");
    assert_eq!(render_with("{% for i in range(900) %}x{% endfor %}", text, &RenderOptions::default()).unwrap().len(), 900);
}

#[test]
fn cancellation_stops_rendering() {
    let cancel = Arc::new(AtomicBool::new(false));
    let budget = Budget::new(LIMITS, cancel.clone()).unwrap();
    let template = Template::new(&Text::from_str("{% for i in range(10) %}{{ i }}{% endfor %}", &budget).unwrap(), &budget).unwrap();
    cancel.store(true, Ordering::Relaxed);
    assert!(matches!(template.render(&Value::Undefined, &RenderOptions::default(), &budget), Err(JinjaError::Budget(Error::Cancelled))));
}

#[test]
fn month_names_come_from_the_host() {
    let now = Some(LocalTime { year: 2026, month: 2, day: 3, hour: 4, minute: 5 });
    let budget = budget(LIMITS);
    let names = |prefix: &str| std::array::from_fn(|index| Text::from_str(&format!("{prefix}{}", index + 1), &budget).unwrap());
    let options = RenderOptions { now, month_names: Some(MonthNames { long: names("long"), short: names("short") }), ..RenderOptions::default() };
    assert_eq!(render_with("{{ strftime_now('%b|%B|%Y-%m-%d %H:%M') }}", LIMITS, &options).unwrap(), "short2|long2|2026-02-03 04:05");
    let clock_only = RenderOptions { now, month_names: None, ..RenderOptions::default() };
    assert!(matches!(render_with("{{ strftime_now('%B') }}", LIMITS, &clock_only), Err(JinjaError::Unsupported("strftime-month-name"))));
    assert_eq!(render_with("{{ strftime_now('%d') }}", LIMITS, &clock_only).unwrap(), "03");
}

/// Case mapping follows the engine's case tables the host names: Rust's own maps every text, Unicode 16 every text
/// but the letters Unicode 17 changed, and other or unprobed tables only ASCII.
#[test]
fn case_mapping_comes_from_the_host_case_tables() {
    let options = |case_tables| RenderOptions { case_tables, ..RenderOptions::default() };
    let declined = |source: &str, tables| {
        matches!(render_with(source, LIMITS, &options(tables)), Err(JinjaError::Unsupported("jinja-case-table")))
    };
    let cased = "{{ 'ꟓé'.upper() }}|{{ 'ÉA'.lower() }}|{{ 'éa'.capitalize() }}|{{ 'É' is upper }}|{{ ['é', 'B'] | sort | join(',') }}";
    assert_eq!(render_with(cased, LIMITS, &options(CaseTables::Native)).unwrap(), "꟒É|éa|Éa|true|B,é");
    assert_eq!(render_with("{{ 'é'.upper() }}", LIMITS, &options(CaseTables::Unicode16)).unwrap(), "É");
    assert!(declined("{{ 'ꟓ'.upper() }}", CaseTables::Unicode16));
    assert_eq!(render_with("{{ 'abc'.upper() }}|{{ 'abc' is lower }}", LIMITS, &RenderOptions::default()).unwrap(), "ABC|true");
    for source in ["{{ 'é'.upper() }}", "{{ 'É'.lower() }}", "{{ 'éa'.capitalize() }}", "{{ 'é' is lower }}", "{{ ['é', 'b'] | sort }}"] {
        assert!(declined(source, CaseTables::Foreign), "{source}");
    }
}

#[test]
fn render_takes_an_object_context() {
    let budget = budget(LIMITS);
    let template = Template::new(&Text::from_str("{{ x }}", &budget).unwrap(), &budget).unwrap();
    assert!(matches!(template.render(&Value::Number(1.0), &RenderOptions::default(), &budget), Err(JinjaError::Unsupported("context-not-object"))));
    assert!(matches!(template.render(&Value::Null, &RenderOptions::default(), &budget), Ok(_)));
}

#[test]
fn errors_map_to_stage_errors() {
    let invalid = |error: JinjaError| match StageError::from(error) {
        StageError::Invalid(name) => name.into_owned(),
        other => panic!("{other:?}"),
    };
    assert_eq!(invalid(render("{% if %}").unwrap_err()), "jinja-syntax");
    assert_eq!(invalid(render("{{ 1 + none }}").unwrap_err()), "jinja-runtime");
    assert_eq!(invalid(render("{{ raise_exception('stop') }}").unwrap_err()), "jinja-raise-exception");
    assert!(matches!(StageError::from(render("{{ [] | first }}").unwrap_err()), StageError::Unsupported(name) if name == "first-of-empty"));
    let shallow = Limits { max_depth: 8, ..LIMITS };
    let recursion = render_with("{% macro f() %}{{ f() }}{% endmacro %}{{ f() }}", shallow, &RenderOptions::default());
    assert!(matches!(StageError::from(recursion.unwrap_err()), StageError::Unsupported(name) if name == "jinja-depth"));
    let work = render_with("{% for i in range(1000) %}x{% endfor %}", Limits { max_work: 100, ..LIMITS }, &RenderOptions::default());
    assert!(matches!(StageError::from(work.unwrap_err()), StageError::Limit(name) if name == "work"));
}

/// Renders one template that runs to a depth limit on a thread with a set stack, in this process. A driver runs it
/// once per size in its own process, since a stack overflow aborts the process: `JINJA_PROBE_STACK` (bytes) and
/// `JINJA_PROBE_KIND`.
#[test]
#[ignore]
fn depth_stack_probe() {
    let (Ok(stack), Ok(kind)) = (std::env::var("JINJA_PROBE_STACK"), std::env::var("JINJA_PROBE_KIND")) else { return };
    let stack: usize = stack.parse().unwrap();
    let source = match kind.as_str() {
        "macro" => "{% macro f(n) %}{{ f(n + 1) }}{% endmacro %}{{ f(0) }}".to_owned(),
        "call" => "{% macro m() %}{% call m() %}x{% endcall %}{% endmacro %}{{ m() }}".to_owned(),
        "nested" => "{% macro f() %}{% for x in [1] %}{% if true %}{{ f()|upper }}{% endif %}{% endfor %}{% endmacro %}{{ f() }}".to_owned(),
        "binary" => format!("{{{{ 1{} }}}}", " + 1".repeat(5000)),
        "filters" => format!("{{{{ 1{} }}}}", "|string".repeat(5000)),
        "elif" => format!("{{% if false %}}{}{{% endif %}}", "{% elif false %}".repeat(5000)),
        "parens" => format!("{{{{ {}1{} }}}}", "(".repeat(5000), ")".repeat(5000)),
        "arrays" => format!("{{{{ {}{} }}}}", "[".repeat(5000), "]".repeat(5000)),
        "blocks" => format!("{}x{}", "{% if true %}".repeat(5000), "{% endif %}".repeat(5000)),
        "value" => "{% set ns = namespace() %}{% set ns.me = ns %}{{ ns }}".to_owned(),
        "walk" => "{% set ns = namespace() %}{% set ns.me = ns %}{% macro f(n) %}{% if n > 0 %}{{ f(n - 1) }}{% else %}{{ ns }}{% endif %}{% endmacro %}{{ f(250) }}".to_owned(),
        "tojson" => "{% set ns = namespace() %}{% set ns.l = [ns] %}{{ ns|tojson(indent=1) }}".to_owned(),
        other => panic!("{other}"),
    };
    let result = std::thread::Builder::new().stack_size(stack).spawn(move || match render(&source) {
        Ok(text) => format!("ok {}", text.len()),
        Err(error) => format!("{error:?}"),
    }).unwrap().join().unwrap();
    println!("probe kind={kind} stack={stack} result={result}");
}
