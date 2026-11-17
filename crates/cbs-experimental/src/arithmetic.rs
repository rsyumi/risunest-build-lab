fn precedence(c: char) -> u8 {
    match c {
        '+' | '-' => 2,
        '*' | '/' | '%' => 3,
        '^' => 4,
        '<' | '>' | '|' | '&' | '≤' | '≥' | '=' | '≠' => 1,
        '!' => 5,
        _ => 0,
    }
}
fn parse(s: &str) -> f64 {
    if let Ok(v) = s.parse() {
        return v;
    }
    for (i, _) in s.char_indices().rev() {
        if let Ok(v) = s[..i].parse() {
            return v;
        }
    }
    f64::NAN
}
fn truth(x: f64) -> bool {
    x != 0.0 && !x.is_nan()
}
fn flat(text: &str) -> f64 {
    let text: String = text.chars().filter(|c| !c.is_whitespace()).collect();
    let text = text
        .replace("&&", "&")
        .replace("||", "|")
        .replace("<=", "≤")
        .replace(">=", "≥")
        .replace("==", "=")
        .replace("!=", "≠");
    let chars: Vec<_> = text.chars().collect();
    let mut tokens = Vec::new();
    let mut last = String::new();
    for (i, &c) in chars.iter().enumerate() {
        if c == '-' && (i == 0 || precedence(chars[i - 1]) > 0) {
            last.push(c)
        } else if precedence(c) > 0 {
            tokens.push(if last.is_empty() {
                "0".into()
            } else {
                std::mem::take(&mut last)
            });
            tokens.push(c.to_string())
        } else {
            last.push(c)
        }
    }
    tokens.push(if last.is_empty() { "0".into() } else { last });
    let mut output = Vec::new();
    let mut ops: Vec<char> = Vec::new();
    for t in tokens {
        if truth(parse(&t)) || t == "0" {
            output.push(t)
        } else {
            let c = t.chars().next().unwrap_or(' ');
            if t.chars().count() == 1 && precedence(c) > 0 {
                while ops.last().is_some_and(|&p| {
                    if c == '!' {
                        precedence(c) < precedence(p)
                    } else {
                        precedence(c) <= precedence(p)
                    }
                }) {
                    output.push(ops.pop().unwrap().to_string())
                }
                ops.push(c)
            }
        }
    }
    while let Some(c) = ops.pop() {
        output.push(c.to_string())
    }
    let mut stack = Vec::new();
    for t in output {
        let n = parse(&t);
        if truth(n) || t == "0" {
            stack.push(n);
            continue;
        }
        let b = stack.pop().unwrap_or(f64::NAN);
        let a = stack.pop().unwrap_or(f64::NAN);
        let bit = |v: bool| if v { 1.0 } else { 0.0 };
        let value = match t.as_str() {
            "+" => a + b,
            "-" => a - b,
            "*" => a * b,
            "/" => a / b,
            "%" => a % b,
            "^" => a.powf(b),
            "<" => bit(a < b),
            ">" => bit(a > b),
            "≤" => bit(a <= b),
            "≥" => bit(a >= b),
            "=" => bit(a == b),
            "≠" => bit(a != b),
            "|" => {
                if truth(a) {
                    a
                } else {
                    b
                }
            }
            "&" => {
                if truth(a) {
                    b
                } else {
                    a
                }
            }
            "!" => bit(!truth(b)),
            _ => continue,
        };
        stack.push(value)
    }
    stack.pop().unwrap_or(0.0)
}
fn number(x: f64) -> String {
    if x.is_nan() {
        "NaN".into()
    } else if x == f64::INFINITY {
        "Infinity".into()
    } else if x == f64::NEG_INFINITY {
        "-Infinity".into()
    } else if x == 0.0 {
        "0".into()
    } else {
        ryu_js::Buffer::new().format(x).to_owned()
    }
}
pub fn evaluate(text: &str) -> Result<String, String> {
    let text = text.to_ascii_lowercase().replace("null", "0");
    if text
        .chars()
        .any(|c| !c.is_ascii_digit() && !c.is_whitespace() && !".+*/%^<>=!&|()-".contains(c))
    {
        return Err("math-domain".into());
    }
    let mut stack = vec![String::new()];
    for c in text.chars() {
        if c == '(' {
            stack.push(String::new())
        } else if c == ')' && stack.len() > 1 {
            let s = stack.pop().unwrap();
            stack.last_mut().unwrap().push_str(&number(flat(&s)))
        } else {
            stack.last_mut().unwrap().push(c)
        }
    }
    Ok(number(flat(&stack.join(""))))
}
