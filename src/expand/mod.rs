mod eval;
mod quote;
mod walk;

pub use walk::expand_keyword;

type Lookup<'a> = &'a dyn Fn(&str) -> Option<String>;
