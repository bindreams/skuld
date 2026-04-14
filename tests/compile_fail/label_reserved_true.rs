// `true` and `false` are reserved by the filter grammar (true/false literals).
// `#[skuld::label]` lowercases the identifier, so `TRUE` produces the name
// `"true"`, which `validate_label_name` rejects at const-eval time.
#[skuld::label]
pub const TRUE: skuld::Label;

fn main() {}
