#[allow(warnings)]
mod bindings;
pub mod eval;
pub mod lexer;
use bindings::exports::demo::calc::arith::Guest;
struct Component;
impl Guest for Component {
    fn eval(expr: String) -> i64 {
        eval::eval(&expr)
    }
}
bindings::export!(Component with_types_in bindings);

#[cfg(test)]
mod tests {
    use super::eval::eval;
    #[test]
    fn adds() {
        assert_eq!(eval("1+2"), 3);
    }
    #[test]
    fn precedence() {
        assert_eq!(eval("2*3+4"), 10);
        assert_eq!(eval("2+3*4"), 14);
    }
    #[test]
    fn parens() {
        assert_eq!(eval("(2+3)*4"), 20);
        assert_eq!(eval("((1+2))*3"), 9);
    }
    #[test]
    fn left_assoc() {
        assert_eq!(eval("10-2-3"), 5);
        assert_eq!(eval("100/5/2"), 10);
    }
    #[test]
    fn unary_minus() {
        assert_eq!(eval("-5+3"), -2);
        assert_eq!(eval("2*-3"), -6);
    }
    #[test]
    fn whitespace() {
        assert_eq!(eval(" 7 * ( 6 + 1 ) "), 49);
    }
    #[test]
    fn mixed() {
        assert_eq!(eval("20/(2+3)"), 4);
        assert_eq!(eval("2*3*4"), 24);
    }
}
