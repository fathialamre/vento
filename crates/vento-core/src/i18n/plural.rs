//! Evaluator for the C-like `plural=` expression of a gettext `Plural-Forms`
//! header, such as `(n != 1)` or Arabic's six-way chain.

/// A compiled plural expression.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Expr(Node);

#[derive(Debug, Clone, PartialEq, Eq)]
enum Node {
    N,
    Num(i128),
    Not(Box<Node>),
    Bin(Op, Box<Node>, Box<Node>),
    Cond(Box<Node>, Box<Node>, Box<Node>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Op {
    Or,
    And,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    Add,
    Sub,
    Mul,
    Div,
    Rem,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tok {
    N,
    Num(i128),
    Op(Op),
    Not,
    Question,
    Colon,
    Open,
    Close,
}

/// Deepest nesting of parentheses and `?:` accepted, so a hostile header
/// cannot overflow the stack.
const MAX_DEPTH: usize = 64;

/// Compiles a plural expression. Supports `n`, integers, `== != < <= > >=
/// && || ! % + - * /`, parentheses and `?:`.
pub fn compile(expr: &str) -> Result<Expr, String> {
    let toks = lex(expr)?;
    let mut p = Parser {
        toks: &toks,
        pos: 0,
        depth: 0,
    };
    let node = p.ternary()?;
    if p.pos != toks.len() {
        return Err(format!("unexpected token after the expression: {expr:?}"));
    }
    Ok(Expr(node))
}

impl Expr {
    /// The plural form index for the count `n`. A negative result is 0; the
    /// caller clamps the index to the number of forms.
    pub fn eval(&self, n: u64) -> usize {
        let v = eval(&self.0, n as i128);
        usize::try_from(v.max(0)).unwrap_or(usize::MAX)
    }
}

fn eval(node: &Node, n: i128) -> i128 {
    match node {
        Node::N => n,
        Node::Num(v) => *v,
        Node::Not(a) => i128::from(eval(a, n) == 0),
        Node::Cond(c, a, b) => {
            if eval(c, n) != 0 {
                eval(a, n)
            } else {
                eval(b, n)
            }
        }
        Node::Bin(op, a, b) => {
            // Short-circuit like C, though no operand has side effects.
            let x = eval(a, n);
            match op {
                Op::Or => i128::from(x != 0 || eval(b, n) != 0),
                Op::And => i128::from(x != 0 && eval(b, n) != 0),
                _ => {
                    let y = eval(b, n);
                    match op {
                        Op::Eq => i128::from(x == y),
                        Op::Ne => i128::from(x != y),
                        Op::Lt => i128::from(x < y),
                        Op::Le => i128::from(x <= y),
                        Op::Gt => i128::from(x > y),
                        Op::Ge => i128::from(x >= y),
                        Op::Add => x.wrapping_add(y),
                        Op::Sub => x.wrapping_sub(y),
                        Op::Mul => x.wrapping_mul(y),
                        Op::Div => x.checked_div(y).unwrap_or(0),
                        Op::Rem => x.checked_rem(y).unwrap_or(0),
                        Op::Or | Op::And => unreachable!(),
                    }
                }
            }
        }
    }
}

fn lex(src: &str) -> Result<Vec<Tok>, String> {
    let b = src.as_bytes();
    let mut i = 0;
    let mut out = Vec::new();
    while i < b.len() {
        let c = b[i];
        let two = b.get(i + 1).copied();
        match c {
            b' ' | b'\t' | b'\r' | b'\n' => i += 1,
            b'0'..=b'9' => {
                let start = i;
                while i < b.len() && b[i].is_ascii_digit() {
                    i += 1;
                }
                let v = src[start..i]
                    .parse::<i128>()
                    .map_err(|_| format!("number too large: {}", &src[start..i]))?;
                out.push(Tok::Num(v));
            }
            b'n' => {
                out.push(Tok::N);
                i += 1;
            }
            b'=' if two == Some(b'=') => {
                out.push(Tok::Op(Op::Eq));
                i += 2;
            }
            b'!' if two == Some(b'=') => {
                out.push(Tok::Op(Op::Ne));
                i += 2;
            }
            b'<' if two == Some(b'=') => {
                out.push(Tok::Op(Op::Le));
                i += 2;
            }
            b'>' if two == Some(b'=') => {
                out.push(Tok::Op(Op::Ge));
                i += 2;
            }
            b'&' if two == Some(b'&') => {
                out.push(Tok::Op(Op::And));
                i += 2;
            }
            b'|' if two == Some(b'|') => {
                out.push(Tok::Op(Op::Or));
                i += 2;
            }
            b'<' => {
                out.push(Tok::Op(Op::Lt));
                i += 1;
            }
            b'>' => {
                out.push(Tok::Op(Op::Gt));
                i += 1;
            }
            b'+' => {
                out.push(Tok::Op(Op::Add));
                i += 1;
            }
            b'-' => {
                out.push(Tok::Op(Op::Sub));
                i += 1;
            }
            b'*' => {
                out.push(Tok::Op(Op::Mul));
                i += 1;
            }
            b'/' => {
                out.push(Tok::Op(Op::Div));
                i += 1;
            }
            b'%' => {
                out.push(Tok::Op(Op::Rem));
                i += 1;
            }
            b'!' => {
                out.push(Tok::Not);
                i += 1;
            }
            b'?' => {
                out.push(Tok::Question);
                i += 1;
            }
            b':' => {
                out.push(Tok::Colon);
                i += 1;
            }
            b'(' => {
                out.push(Tok::Open);
                i += 1;
            }
            b')' => {
                out.push(Tok::Close);
                i += 1;
            }
            _ => {
                return Err(format!(
                    "unexpected character {:?} in plural expression {src:?}",
                    src[i..].chars().next().unwrap_or('?')
                ));
            }
        }
    }
    if out.is_empty() {
        return Err("empty plural expression".to_string());
    }
    Ok(out)
}

struct Parser<'a> {
    toks: &'a [Tok],
    pos: usize,
    depth: usize,
}

impl Parser<'_> {
    fn peek(&self) -> Option<Tok> {
        self.toks.get(self.pos).copied()
    }

    fn eat(&mut self, t: Tok) -> bool {
        if self.peek() == Some(t) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn enter(&mut self) -> Result<(), String> {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            return Err("plural expression nests too deeply".to_string());
        }
        Ok(())
    }

    /// `cond ? a : b`, right associative.
    fn ternary(&mut self) -> Result<Node, String> {
        self.enter()?;
        let cond = self.binary(0)?;
        let node = if self.eat(Tok::Question) {
            let a = self.ternary()?;
            if !self.eat(Tok::Colon) {
                return Err("expected ':' in plural expression".to_string());
            }
            let b = self.ternary()?;
            Node::Cond(Box::new(cond), Box::new(a), Box::new(b))
        } else {
            cond
        };
        self.depth -= 1;
        Ok(node)
    }

    /// Binary operators by precedence level, loosest first.
    fn binary(&mut self, level: usize) -> Result<Node, String> {
        const LEVELS: &[&[Op]] = &[
            &[Op::Or],
            &[Op::And],
            &[Op::Eq, Op::Ne],
            &[Op::Lt, Op::Le, Op::Gt, Op::Ge],
            &[Op::Add, Op::Sub],
            &[Op::Mul, Op::Div, Op::Rem],
        ];
        if level == LEVELS.len() {
            return self.unary();
        }
        let mut left = self.binary(level + 1)?;
        while let Some(Tok::Op(op)) = self.peek() {
            if !LEVELS[level].contains(&op) {
                break;
            }
            self.pos += 1;
            let right = self.binary(level + 1)?;
            left = Node::Bin(op, Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn unary(&mut self) -> Result<Node, String> {
        if self.eat(Tok::Not) {
            self.enter()?;
            let inner = self.unary()?;
            self.depth -= 1;
            return Ok(Node::Not(Box::new(inner)));
        }
        match self.peek() {
            Some(Tok::N) => {
                self.pos += 1;
                Ok(Node::N)
            }
            Some(Tok::Num(v)) => {
                self.pos += 1;
                Ok(Node::Num(v))
            }
            Some(Tok::Open) => {
                self.pos += 1;
                let inner = self.ternary()?;
                if !self.eat(Tok::Close) {
                    return Err("missing ')' in plural expression".to_string());
                }
                Ok(inner)
            }
            _ => Err("incomplete plural expression".to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arabic_has_six_forms() {
        let e = compile(
            "(n==0 ? 0 : n==1 ? 1 : n==2 ? 2 : n%100>=3 && n%100<=10 ? 3 : n%100>=11 ? 4 : 5)",
        )
        .unwrap();
        let got: Vec<usize> = [0, 1, 2, 3, 10, 11, 99, 100, 101, 102, 111]
            .iter()
            .map(|&n| e.eval(n))
            .collect();
        assert_eq!(got, vec![0, 1, 2, 3, 3, 4, 4, 5, 5, 5, 4]);
    }

    #[test]
    fn english_rule() {
        let e = compile("(n != 1)").unwrap();
        assert_eq!((e.eval(1), e.eval(0), e.eval(2)), (0, 1, 1));
    }

    #[test]
    fn operators_and_precedence() {
        assert_eq!(compile("1 + 2 * 3").unwrap().eval(0), 7);
        assert_eq!(compile("!(n == 1)").unwrap().eval(1), 0);
        assert_eq!(compile("n > 1 || n == 0").unwrap().eval(0), 1);
        assert_eq!(compile("n % 0").unwrap().eval(5), 0);
        // A huge count must not overflow or panic.
        let _ = compile("n * n + n").unwrap().eval(u64::MAX);
    }

    #[test]
    fn rejects_bad_expressions() {
        assert!(compile("").is_err());
        assert!(compile("n ==").is_err());
        assert!(compile("(n").is_err());
        assert!(compile("n ? 1").is_err());
        assert!(compile("n $ 2").is_err());
        assert!(compile("n 2").is_err());
    }
}
