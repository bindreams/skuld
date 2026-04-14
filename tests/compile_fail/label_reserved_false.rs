// `true` and `false` are reserved by the filter grammar (true/false literals).
// `#[skuld::label]` lowercases the identifier, so `FALSE` produces the name
// `"false"`, which `validate_label_name` rejects at const-eval time.
#[skuld::label]
pub const FALSE: skuld::Label;

fn main() {}
