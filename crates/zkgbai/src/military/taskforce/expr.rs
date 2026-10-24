//! Want expressions: how many of a role a force wants, written over what
//! the force has.
//!
//! `ceil(0.5 * cores)`, `floor(0.2 * guns)`, `value(core) > 5000 ? 1 : 0`:
//! numbers, `+ - * /`, comparisons and `&& || !` (true is 1, false 0),
//! `a ? b : c`, parentheses, and the functions `ceil floor round min max
//! abs`. Names are what the force has: `cores` (its core units), `value`
//! (its metal), a role's name (how many it has of the role); `count(x)` and
//! `value(x)` take `core` or a role's name.

use std::fmt;

#[derive(Clone, Debug, PartialEq)]
enum Node {
    Num(f32),
    /// `cores`, `value` or a role's name.
    Name(String),
    /// `count(x)` or `value(x)`.
    Of(bool, String),
    Call(Func, Vec<Node>),
    Neg(Box<Node>),
    Not(Box<Node>),
    Bin(Op, Box<Node>, Box<Node>),
    If(Box<Node>, Box<Node>, Box<Node>),
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Func {
    Ceil,
    Floor,
    Round,
    Min,
    Max,
    Abs,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Op {
    Add,
    Sub,
    Mul,
    Div,
    Lt,
    Le,
    Gt,
    Ge,
    Eq,
    Ne,
    And,
    Or,
}

/// A parsed want expression, with its source.
#[derive(Clone, PartialEq)]
pub struct Expr {
    src: String,
    root: Node,
}

impl fmt::Debug for Expr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}", self.src)
    }
}

/// What a force has, for its wants: how many cores and their metal, and
/// how many of each role and their metal (in the doctrine's role order).
#[derive(Clone, Debug, Default)]
pub struct Force {
    pub cores: usize,
    pub core_value: f32,
    /// By role: name, how many, their metal.
    pub roles: Vec<(String, usize, f32)>,
}

impl Force {
    fn count(&self, name: &str) -> Option<f32> {
        if name == "core" || name == "cores" {
            return Some(self.cores as f32);
        }
        self.roles.iter().find(|r| r.0 == name).map(|r| r.1 as f32)
    }

    fn value_of(&self, name: &str) -> Option<f32> {
        if name == "core" || name == "cores" {
            return Some(self.core_value);
        }
        self.roles.iter().find(|r| r.0 == name).map(|r| r.2)
    }

    fn value(&self) -> f32 {
        self.core_value + self.roles.iter().map(|r| r.2).sum::<f32>()
    }
}

impl Expr {
    /// Parse `src`; names are checked when the doctrine is (see
    /// [`Expr::names`]).
    pub fn parse(src: &str) -> Result<Expr, String> {
        let tokens = lex(src)?;
        let mut p = Parser { tokens, at: 0 };
        let root = p.ternary()?;
        if p.at < p.tokens.len() {
            return Err(format!("want '{src}': unexpected {}", p.tokens[p.at]));
        }
        Ok(Expr { src: src.trim().to_owned(), root })
    }

    /// `n` of the force's cores: what `per_core` used to say.
    pub fn per_core(n: f32) -> Expr {
        Expr::parse(&format!("ceil({n} * cores)")).expect("per_core")
    }

    pub fn source(&self) -> &str {
        &self.src
    }

    /// The names it reads (other than `cores` and `value`), for checking
    /// against the doctrine's roles.
    pub fn names(&self) -> Vec<&str> {
        fn walk<'a>(n: &'a Node, out: &mut Vec<&'a str>) {
            match n {
                Node::Name(s) if s != "cores" && s != "value" && s != "core" => out.push(s),
                Node::Of(_, s) if s != "core" && s != "cores" => out.push(s),
                Node::Call(_, args) => args.iter().for_each(|a| walk(a, out)),
                Node::Neg(a) | Node::Not(a) => walk(a, out),
                Node::Bin(_, a, b) => {
                    walk(a, out);
                    walk(b, out);
                }
                Node::If(c, a, b) => {
                    walk(c, out);
                    walk(a, out);
                    walk(b, out);
                }
                _ => {}
            }
        }
        let mut out = Vec::new();
        walk(&self.root, &mut out);
        out
    }

    /// Its value for `force` (names it does not know read 0).
    pub fn eval(&self, force: &Force) -> f32 {
        eval(&self.root, force)
    }

    /// How many it wants of `force`: its value rounded down, and never
    /// below 0.
    pub fn want(&self, force: &Force) -> usize {
        let v = self.eval(force);
        if v.is_finite() { v.max(0.0).floor() as usize } else { 0 }
    }
}

fn truth(b: bool) -> f32 {
    if b { 1.0 } else { 0.0 }
}

fn eval(n: &Node, f: &Force) -> f32 {
    match n {
        Node::Num(x) => *x,
        Node::Name(s) if s == "value" => f.value(),
        Node::Name(s) => f.count(s).unwrap_or(0.0),
        Node::Of(false, s) => f.count(s).unwrap_or(0.0),
        Node::Of(true, s) => f.value_of(s).unwrap_or(0.0),
        Node::Neg(a) => -eval(a, f),
        Node::Not(a) => truth(eval(a, f) == 0.0),
        Node::Call(func, args) => {
            let v: Vec<f32> = args.iter().map(|a| eval(a, f)).collect();
            match func {
                Func::Ceil => v[0].ceil(),
                Func::Floor => v[0].floor(),
                Func::Round => v[0].round(),
                Func::Abs => v[0].abs(),
                Func::Min => v.iter().copied().fold(f32::INFINITY, f32::min),
                Func::Max => v.iter().copied().fold(f32::NEG_INFINITY, f32::max),
            }
        }
        Node::Bin(op, a, b) => {
            let (x, y) = (eval(a, f), eval(b, f));
            match op {
                Op::Add => x + y,
                Op::Sub => x - y,
                Op::Mul => x * y,
                Op::Div => {
                    if y == 0.0 {
                        0.0
                    } else {
                        x / y
                    }
                }
                Op::Lt => truth(x < y),
                Op::Le => truth(x <= y),
                Op::Gt => truth(x > y),
                Op::Ge => truth(x >= y),
                Op::Eq => truth(x == y),
                Op::Ne => truth(x != y),
                Op::And => truth(x != 0.0 && y != 0.0),
                Op::Or => truth(x != 0.0 || y != 0.0),
            }
        }
        Node::If(c, a, b) => {
            if eval(c, f) != 0.0 {
                eval(a, f)
            } else {
                eval(b, f)
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
enum Tok {
    Num(f32),
    Name(String),
    Sym(&'static str),
}

impl fmt::Display for Tok {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Tok::Num(x) => write!(f, "{x}"),
            Tok::Name(s) => write!(f, "'{s}'"),
            Tok::Sym(s) => write!(f, "'{s}'"),
        }
    }
}

const SYMS: [&str; 18] =
    ["<=", ">=", "==", "!=", "&&", "||", "<", ">", "+", "-", "*", "/", "(", ")", ",", "?", ":", "!"];

fn lex(src: &str) -> Result<Vec<Tok>, String> {
    let s: Vec<char> = src.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < s.len() {
        let c = s[i];
        if c.is_whitespace() {
            i += 1;
        } else if c.is_ascii_digit() || c == '.' {
            let start = i;
            while i < s.len() && (s[i].is_ascii_digit() || s[i] == '.') {
                i += 1;
            }
            let text: String = s[start..i].iter().collect();
            out.push(Tok::Num(text.parse().map_err(|_| format!("want '{src}': bad number {text}"))?));
        } else if c.is_alphabetic() || c == '_' {
            let start = i;
            while i < s.len() && (s[i].is_alphanumeric() || s[i] == '_') {
                i += 1;
            }
            out.push(Tok::Name(s[start..i].iter().collect()));
        } else {
            let rest: String = s[i..s.len().min(i + 2)].iter().collect();
            let Some(sym) = SYMS.iter().find(|sym| rest.starts_with(**sym)) else {
                return Err(format!("want '{src}': unexpected '{c}'"));
            };
            out.push(Tok::Sym(sym));
            i += sym.len();
        }
    }
    Ok(out)
}

struct Parser {
    tokens: Vec<Tok>,
    at: usize,
}

impl Parser {
    fn peek(&self) -> Option<&Tok> {
        self.tokens.get(self.at)
    }

    fn eat(&mut self, sym: &str) -> bool {
        if self.peek()
            == Some(&Tok::Sym(match SYMS.iter().find(|s| **s == sym) {
                Some(s) => s,
                None => return false,
            }))
        {
            self.at += 1;
            true
        } else {
            false
        }
    }

    fn expect(&mut self, sym: &str) -> Result<(), String> {
        if self.eat(sym) {
            Ok(())
        } else {
            Err(match self.peek() {
                Some(t) => format!("want: expected '{sym}', found {t}"),
                None => format!("want: expected '{sym}' at the end"),
            })
        }
    }

    fn ternary(&mut self) -> Result<Node, String> {
        let c = self.binary(0)?;
        if self.eat("?") {
            let a = self.ternary()?;
            self.expect(":")?;
            let b = self.ternary()?;
            return Ok(Node::If(Box::new(c), Box::new(a), Box::new(b)));
        }
        Ok(c)
    }

    /// Binary operators by precedence level: `||`, `&&`, comparisons,
    /// `+ -`, `* /`.
    fn binary(&mut self, level: usize) -> Result<Node, String> {
        const LEVELS: [&[(&str, Op)]; 5] = [
            &[("||", Op::Or)],
            &[("&&", Op::And)],
            &[("<=", Op::Le), (">=", Op::Ge), ("==", Op::Eq), ("!=", Op::Ne), ("<", Op::Lt), (">", Op::Gt)],
            &[("+", Op::Add), ("-", Op::Sub)],
            &[("*", Op::Mul), ("/", Op::Div)],
        ];
        if level == LEVELS.len() {
            return self.unary();
        }
        let mut lhs = self.binary(level + 1)?;
        'more: loop {
            for &(sym, op) in LEVELS[level] {
                if self.eat(sym) {
                    let rhs = self.binary(level + 1)?;
                    lhs = Node::Bin(op, Box::new(lhs), Box::new(rhs));
                    continue 'more;
                }
            }
            return Ok(lhs);
        }
    }

    fn unary(&mut self) -> Result<Node, String> {
        if self.eat("-") {
            return Ok(Node::Neg(Box::new(self.unary()?)));
        }
        if self.eat("!") {
            return Ok(Node::Not(Box::new(self.unary()?)));
        }
        self.atom()
    }

    fn atom(&mut self) -> Result<Node, String> {
        match self.peek().cloned() {
            Some(Tok::Num(x)) => {
                self.at += 1;
                Ok(Node::Num(x))
            }
            Some(Tok::Sym("(")) => {
                self.at += 1;
                let e = self.ternary()?;
                self.expect(")")?;
                Ok(e)
            }
            Some(Tok::Name(name)) => {
                self.at += 1;
                if !self.eat("(") {
                    return Ok(Node::Name(name));
                }
                if name == "count" || name == "value" {
                    let Some(Tok::Name(arg)) = self.peek().cloned() else {
                        return Err(format!("want: {name}() takes 'core' or a role's name"));
                    };
                    self.at += 1;
                    self.expect(")")?;
                    return Ok(Node::Of(name == "value", arg));
                }
                let func = match name.as_str() {
                    "ceil" => Func::Ceil,
                    "floor" => Func::Floor,
                    "round" => Func::Round,
                    "min" => Func::Min,
                    "max" => Func::Max,
                    "abs" => Func::Abs,
                    _ => return Err(format!("want: no function '{name}' (ceil floor round min max abs count value)")),
                };
                let mut args = vec![self.ternary()?];
                while self.eat(",") {
                    args.push(self.ternary()?);
                }
                self.expect(")")?;
                let one = matches!(func, Func::Ceil | Func::Floor | Func::Round | Func::Abs);
                if one && args.len() != 1 {
                    return Err(format!("want: {name}() takes one argument"));
                }
                Ok(Node::Call(func, args))
            }
            Some(t) => Err(format!("want: unexpected {t}")),
            None => Err("want: unexpected end".into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn force(cores: usize, core_value: f32) -> Force {
        Force { cores, core_value, roles: vec![("guns".into(), 7, 2100.0), ("eraser".into(), 0, 0.0)] }
    }

    fn want(src: &str, f: &Force) -> usize {
        Expr::parse(src).unwrap().want(f)
    }

    #[test]
    fn wants_read_what_the_force_has() {
        let f = force(3, 2100.0);
        assert_eq!(want("ceil(0.5 * cores)", &f), 2);
        assert_eq!(want("0.5 * cores", &f), 1);
        assert_eq!(want("floor(0.2 * guns)", &f), 1);
        assert_eq!(want("ceil(0.3 * count(guns))", &f), 3);
        // One Eraser, and only one, past 5000 metal of guns.
        assert_eq!(want("value(guns) > 5000 ? 1 : 0", &f), 0);
        let rich = Force { roles: vec![("guns".into(), 12, 6000.0)], ..f.clone() };
        assert_eq!(want("value(guns) > 5000 ? 1 : 0", &rich), 1);
        assert_eq!(want("min(1, value(guns) > 5000)", &rich), 1);
        assert_eq!(want("value > 4000 && cores >= 3", &f), 1);
        assert_eq!(want("1 + 2 * 3 - 4 / 2", &f), 5);
        assert_eq!(want("-2", &f), 0);
        assert_eq!(want("cores / 0", &f), 0);
        assert_eq!(want("!(cores > 5) ? 2 : 1", &f), 2);
    }

    #[test]
    fn bad_wants_say_why() {
        assert!(Expr::parse("ceil(0.5 * cores").unwrap_err().contains("expected ')'"));
        assert!(Expr::parse("sqrt(cores)").unwrap_err().contains("no function"));
        assert!(Expr::parse("cores $ 2").unwrap_err().contains("unexpected"));
        assert!(Expr::parse("ceil(1, 2)").unwrap_err().contains("one argument"));
        assert!(Expr::parse("cores 2").is_err());
        assert_eq!(Expr::parse("floor(0.2 * guns) + eraser").unwrap().names(), vec!["guns", "eraser"]);
        assert_eq!(Expr::per_core(1.5).source(), "ceil(1.5 * cores)");
    }
}
