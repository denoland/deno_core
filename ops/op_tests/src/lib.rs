#[macro_export]
macro_rules! prelude {
    () => {
        use deno_ops::op2;

        pub fn main() {}
    };
}

#[cfg(test)]
mod tests {
    #[test]
    pub fn test() {

        let t = trybuild::TestCases::new();
        t.pass("../op2/test_cases/sync/add.rs");
    }
}
