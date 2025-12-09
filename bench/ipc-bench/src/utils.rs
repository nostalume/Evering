#[macro_export]
macro_rules! with {
    ($buf:ident = $expr:expr) => {{
        let r;
        (r, $buf) = $expr;
        r
    }};
}
