mod eval;
mod quote;
pub mod walk;

type Lookup<'a> = &'a dyn Fn(&str) -> Option<String>;
