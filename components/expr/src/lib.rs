#[allow(warnings)]
mod bindings;
pub mod lexer;
pub mod eval;
use bindings::exports::demo::expr::language::Guest;
struct Component;
impl Guest for Component {
    fn eval(src: String) -> i64 { eval::eval(&src) }
}
bindings::export!(Component with_types_in bindings);

#[cfg(test)]
mod tests {
    use super::eval::eval;
    #[test] fn arithmetic_precedence() { assert_eq!(eval("1+2*3"), 7); assert_eq!(eval("(1+2)*3"), 9); }
    #[test] fn power_is_right_associative() { assert_eq!(eval("2**3**2"), 512); assert_eq!(eval("2**0"), 1); }
    #[test] fn power_binds_tighter_than_unary_minus() { assert_eq!(eval("-2**2"), -4); }
    #[test] fn div_mod_left_assoc() { assert_eq!(eval("6/2*3"), 9); assert_eq!(eval("2*3%4"), 2); assert_eq!(eval("10%3"), 1); }
    #[test] fn subtraction_left_assoc() { assert_eq!(eval("100-10-5"), 85); }
    #[test] fn comparisons_below_arithmetic() { assert_eq!(eval("2+3==5"), 1); assert_eq!(eval("5>3"), 1); assert_eq!(eval("3>5"), 0); }
    #[test] fn comparison_ops() { assert_eq!(eval("2<=2"), 1); assert_eq!(eval("2!=3"), 1); assert_eq!(eval("4>=5"), 0); }
    #[test] fn logical() { assert_eq!(eval("1&&0"), 0); assert_eq!(eval("1||0"), 1); assert_eq!(eval("0||0||1"), 1); }
    #[test] fn not_is_unary() { assert_eq!(eval("!0"), 1); assert_eq!(eval("!5"), 0); assert_eq!(eval("!1||1"), 1); }
    #[test] fn precedence_across_levels() { assert_eq!(eval("1+2>2 && 3<4"), 1); }
    #[test] fn unary_and_parens() { assert_eq!(eval("-(3+4)"), -7); }
}
