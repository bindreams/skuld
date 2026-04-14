use proc_macro::TokenStream;
use quote::{format_ident, quote};
use syn::parse::{Nothing, Parse, ParseStream};
use syn::{
    bracketed, parse_macro_input, punctuated::Punctuated, Attribute, Expr, FnArg, Ident, ItemFn, Lit, LitStr, Path,
    ReturnType, Token, Type, Visibility,
};

// #[skuld::test] argument parsing =================================================================

/// Parsed arguments for `#[skuld::test(...)]`.
#[derive(Default)]
struct TestArgs {
    requires: Vec<Path>,
    name: Option<String>,
    labels: Option<Vec<Path>>,
    ignore: IgnoreArg,
    serial: Option<String>,
    serial_labels: Vec<Ident>,
    should_panic: ShouldPanicArg,
}

#[derive(Default)]
enum IgnoreArg {
    #[default]
    No,
    Yes,
    WithReason(String),
}

#[derive(Default)]
enum ShouldPanicArg {
    #[default]
    No,
    Yes,
    WithMessage(String),
}

impl Parse for TestArgs {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let mut args = TestArgs::default();

        if input.is_empty() {
            return Ok(args);
        }

        loop {
            let key: Ident = input.parse()?;
            match key.to_string().as_str() {
                "requires" => {
                    let _eq: Token![=] = input.parse()?;
                    let content;
                    bracketed!(content in input);
                    args.requires = Punctuated::<Path, Token![,]>::parse_terminated(&content)?
                        .into_iter()
                        .collect();
                }
                "name" => {
                    let _eq: Token![=] = input.parse()?;
                    let lit: LitStr = input.parse()?;
                    args.name = Some(lit.value());
                }
                "labels" => {
                    let _eq: Token![=] = input.parse()?;
                    let content;
                    bracketed!(content in input);
                    args.labels = Some(
                        Punctuated::<Path, Token![,]>::parse_terminated(&content)?
                            .into_iter()
                            .collect(),
                    );
                }
                "ignore" => {
                    if input.peek(Token![=]) {
                        let _eq: Token![=] = input.parse()?;
                        let lit: LitStr = input.parse()?;
                        args.ignore = IgnoreArg::WithReason(lit.value());
                    } else {
                        args.ignore = IgnoreArg::Yes;
                    }
                }
                "serial" => {
                    if input.peek(Token![=]) {
                        let _eq: Token![=] = input.parse()?;
                        let (expr_str, label_idents) = parse_serial_expr(input)?;
                        args.serial = Some(expr_str);
                        args.serial_labels = label_idents;
                    } else {
                        args.serial = Some("*".to_string());
                    }
                }
                "should_panic" => {
                    if input.peek(Token![=]) {
                        let _eq: Token![=] = input.parse()?;
                        let lit: LitStr = input.parse()?;
                        args.should_panic = ShouldPanicArg::WithMessage(lit.value());
                    } else {
                        args.should_panic = ShouldPanicArg::Yes;
                    }
                }
                other => {
                    return Err(syn::Error::new(
                        key.span(),
                        format!("unknown argument `{other}`; expected requires, name, labels, ignore, serial, or should_panic"),
                    ));
                }
            }

            if input.is_empty() {
                break;
            }
            let _comma: Token![,] = input.parse()?;
            if input.is_empty() {
                break; // trailing comma
            }
        }

        Ok(args)
    }
}

// Outer attribute absorption =====================================================================

/// Parse `#[ignore]` or `#[ignore = "reason"]` from an outer attribute.
fn absorb_ignore_attr(attr: &syn::Attribute) -> syn::Result<IgnoreArg> {
    match &attr.meta {
        syn::Meta::Path(_) => Ok(IgnoreArg::Yes),
        syn::Meta::NameValue(nv) => match &nv.value {
            Expr::Lit(expr_lit) => match &expr_lit.lit {
                Lit::Str(s) => Ok(IgnoreArg::WithReason(s.value())),
                _ => Err(syn::Error::new_spanned(
                    &nv.value,
                    "#[ignore = ...] expects a string literal",
                )),
            },
            _ => Err(syn::Error::new_spanned(
                &nv.value,
                "#[ignore = ...] expects a string literal",
            )),
        },
        _ => Err(syn::Error::new_spanned(
            attr,
            "expected #[ignore] or #[ignore = \"reason\"]",
        )),
    }
}

/// Helper for parsing the `expected = "msg"` inside `#[should_panic(expected = "msg")]`.
struct ShouldPanicExpected {
    value: String,
}

impl Parse for ShouldPanicExpected {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let key: Ident = input.parse()?;
        if key != "expected" {
            return Err(syn::Error::new(
                key.span(),
                format!("expected `expected`, found `{key}`"),
            ));
        }
        let _eq: Token![=] = input.parse()?;
        let lit: LitStr = input.parse()?;
        Ok(ShouldPanicExpected { value: lit.value() })
    }
}

/// Parse `#[should_panic]` or `#[should_panic(expected = "msg")]` from an outer attribute.
fn absorb_should_panic_attr(attr: &syn::Attribute) -> syn::Result<ShouldPanicArg> {
    match &attr.meta {
        syn::Meta::Path(_) => Ok(ShouldPanicArg::Yes),
        syn::Meta::List(list) => {
            let parsed: ShouldPanicExpected = syn::parse2(list.tokens.clone())?;
            Ok(ShouldPanicArg::WithMessage(parsed.value))
        }
        _ => Err(syn::Error::new_spanned(
            attr,
            "expected #[should_panic] or #[should_panic(expected = \"message\")]",
        )),
    }
}

// #[skuld::fixture] argument parsing ==============================================================

/// Parsed arguments for `#[skuld::fixture(...)]`.
#[derive(Default)]
struct FixtureArgs {
    requires: Vec<Path>,
    scope: Option<Ident>,
    name: Option<String>,
    deref: bool,
    serial: Option<String>,
    serial_labels: Vec<Ident>,
}

impl Parse for FixtureArgs {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let mut args = FixtureArgs::default();

        if input.is_empty() {
            return Ok(args);
        }

        loop {
            let key: Ident = input.parse()?;
            match key.to_string().as_str() {
                "requires" => {
                    let _eq: Token![=] = input.parse()?;
                    let content;
                    bracketed!(content in input);
                    args.requires = Punctuated::<Path, Token![,]>::parse_terminated(&content)?
                        .into_iter()
                        .collect();
                }
                "scope" => {
                    let _eq: Token![=] = input.parse()?;
                    let scope: Ident = input.parse()?;
                    let s = scope.to_string();
                    if s != "variable" && s != "test" && s != "process" {
                        return Err(syn::Error::new(
                            scope.span(),
                            format!("unknown scope `{s}`; expected variable, test, or process"),
                        ));
                    }
                    args.scope = Some(scope);
                }
                "name" => {
                    let _eq: Token![=] = input.parse()?;
                    let lit: LitStr = input.parse()?;
                    args.name = Some(lit.value());
                }
                "deref" => {
                    args.deref = true;
                }
                "serial" => {
                    if input.peek(Token![=]) {
                        let _eq: Token![=] = input.parse()?;
                        let (expr_str, label_idents) = parse_serial_expr(input)?;
                        args.serial = Some(expr_str);
                        args.serial_labels = label_idents;
                    } else {
                        args.serial = Some("*".to_string());
                    }
                }
                other => {
                    return Err(syn::Error::new(
                        key.span(),
                        format!("unknown argument `{other}`; expected requires, scope, name, deref, or serial"),
                    ));
                }
            }

            if input.is_empty() {
                break;
            }
            let _comma: Token![,] = input.parse()?;
            if input.is_empty() {
                break;
            }
        }

        Ok(args)
    }
}

// Serial expression parsing =======================================================================

/// Parse a serial filter expression from proc-macro tokens.
/// Returns (canonical_expression_string, list_of_label_identifiers).
fn parse_serial_expr(input: ParseStream) -> syn::Result<(String, Vec<Ident>)> {
    let mut labels = Vec::new();
    let expr = parse_serial_or(input, &mut labels)?;
    Ok((expr, labels))
}

fn parse_serial_or(input: ParseStream, labels: &mut Vec<Ident>) -> syn::Result<String> {
    let mut left = parse_serial_and(input, labels)?;
    while input.peek(Token![|]) {
        let _: Token![|] = input.parse()?;
        let right = parse_serial_and(input, labels)?;
        left = format!("{left} | {right}");
    }
    Ok(left)
}

fn parse_serial_and(input: ParseStream, labels: &mut Vec<Ident>) -> syn::Result<String> {
    let mut left = parse_serial_not(input, labels)?;
    while input.peek(Token![&]) {
        let _: Token![&] = input.parse()?;
        let right = parse_serial_not(input, labels)?;
        left = format!("{left} & {right}");
    }
    Ok(left)
}

fn parse_serial_not(input: ParseStream, labels: &mut Vec<Ident>) -> syn::Result<String> {
    if input.peek(Token![!]) {
        let _: Token![!] = input.parse()?;
        let inner = parse_serial_not(input, labels)?;
        Ok(format!("!{inner}"))
    } else {
        parse_serial_primary(input, labels)
    }
}

fn parse_serial_primary(input: ParseStream, labels: &mut Vec<Ident>) -> syn::Result<String> {
    if input.peek(syn::token::Paren) {
        let content;
        syn::parenthesized!(content in input);
        let inner = parse_serial_or(&content, labels)?;
        Ok(format!("({inner})"))
    } else {
        let ident: Ident = input.parse()?;
        labels.push(ident.clone());
        Ok(ident.to_string())
    }
}

// #[fixture] parameter parsing ====================================================================

/// Parsed info for a `#[fixture]` / `#[fixture(name)]` parameter.
struct FixtureParam {
    /// The parameter's binding pattern (e.g. `dir`).
    binding: syn::Pat,
    /// The fixture name to look up (param name or explicit).
    fixture_name: String,
    /// The target type for the cast (param type stripped of `&`).
    target_ty: Type,
    /// The full parameter type (e.g. `&Path`).
    param_ty: Box<Type>,
}

/// Parse a `#[fixture]` or `#[fixture(name)]` attribute on a function parameter.
fn parse_fixture_param(attr: &syn::Attribute, pat_type: &syn::PatType) -> FixtureParam {
    let binding = (*pat_type.pat).clone();
    let param_ty = pat_type.ty.clone();
    let target_ty = strip_reference(&param_ty);

    // Fixture name: from #[fixture(name)] or from the parameter name.
    let fixture_name = parse_fixture_name_arg(attr).unwrap_or_else(|| {
        // Use the parameter's binding pattern as the name.
        binding_to_name(&binding)
    });

    FixtureParam {
        binding,
        fixture_name,
        target_ty,
        param_ty,
    }
}

/// Extract the name argument from `#[fixture(name)]`. Returns `None` for bare `#[fixture]`.
fn parse_fixture_name_arg(attr: &syn::Attribute) -> Option<String> {
    match &attr.meta {
        syn::Meta::List(list) => {
            let ident: Ident = syn::parse2(list.tokens.clone()).ok()?;
            Some(ident.to_string())
        }
        _ => None,
    }
}

/// Extract a simple identifier name from a binding pattern.
fn binding_to_name(pat: &syn::Pat) -> String {
    match pat {
        syn::Pat::Ident(ident) => ident.ident.to_string(),
        _ => panic!("#[fixture] parameter must be a simple identifier binding"),
    }
}

// #[skuld::test] ==================================================================================

/// Register a test function with the skuld harness.
///
/// Parameters annotated with `#[fixture]` or `#[fixture(name)]` are injected
/// from the name-based fixture registry. The fixture name is the parameter name
/// or the explicit name in `#[fixture(name)]`.
///
/// Standard `#[ignore]` and `#[should_panic]` outer attributes are also
/// accepted and behave identically to their macro-argument equivalents.
/// These must appear **after** `#[skuld::test]`, not before it.
///
/// ```ignore
/// #[skuld::test(requires = [preconditions::valgrind], labels = [SLOW])]
/// fn my_test(#[fixture(temp_dir)] dir: &Path) { /* ... */ }
///
/// #[skuld::test]
/// #[ignore = "not yet implemented"]
/// fn wip() { /* ... */ }
///
/// #[skuld::test]
/// #[should_panic(expected = "out of range")]
/// fn panics_with_message() { my_function(too_large); }
/// ```
#[proc_macro_attribute]
pub fn test(attr: TokenStream, item: TokenStream) -> TokenStream {
    let mut args = parse_macro_input!(attr as TestArgs);
    let func = parse_macro_input!(item as ItemFn);
    expand_test_def(&mut args, func)
}

fn expand_test_def(args: &mut TestArgs, func: ItemFn) -> TokenStream {
    // Absorb outer attributes: #[test], #[ignore], #[should_panic].
    let mut outer_ignore: Option<&syn::Attribute> = None;
    let mut outer_should_panic: Option<&syn::Attribute> = None;

    for attr in &func.attrs {
        if attr.path().is_ident("test") {
            return syn::Error::new_spanned(attr, "remove #[test] — #[skuld::test] already registers this function")
                .to_compile_error()
                .into();
        } else if attr.path().is_ident("ignore") {
            if outer_ignore.is_some() {
                return syn::Error::new_spanned(attr, "duplicate #[ignore] attribute")
                    .to_compile_error()
                    .into();
            }
            outer_ignore = Some(attr);
        } else if attr.path().is_ident("should_panic") {
            if outer_should_panic.is_some() {
                return syn::Error::new_spanned(attr, "duplicate #[should_panic] attribute")
                    .to_compile_error()
                    .into();
            }
            outer_should_panic = Some(attr);
        }
    }

    // Merge outer #[ignore] with macro arguments.
    if let Some(attr) = outer_ignore {
        if !matches!(args.ignore, IgnoreArg::No) {
            return syn::Error::new_spanned(
                attr,
                "conflicting `ignore`: remove either #[ignore] or the `ignore` argument from #[skuld::test(...)]",
            )
            .to_compile_error()
            .into();
        }
        match absorb_ignore_attr(attr) {
            Ok(val) => args.ignore = val,
            Err(e) => return e.to_compile_error().into(),
        }
    }

    // Merge outer #[should_panic] with macro arguments.
    if let Some(attr) = outer_should_panic {
        if !matches!(args.should_panic, ShouldPanicArg::No) {
            return syn::Error::new_spanned(
                attr,
                "conflicting `should_panic`: remove either #[should_panic] or the `should_panic` argument from #[skuld::test(...)]",
            )
            .to_compile_error()
            .into();
        }
        match absorb_should_panic_attr(attr) {
            Ok(val) => args.should_panic = val,
            Err(e) => return e.to_compile_error().into(),
        }
    }

    let name = &func.sig.ident;
    let name_str = name.to_string();

    let display_name_expr = build_display_name(&args.name);
    let labels_explicit = args.labels.is_some();
    let label_paths: Vec<Path> = args.labels.take().unwrap_or_default();
    let ignore_expr = match &args.ignore {
        IgnoreArg::No => quote! { ::skuld::Ignore::No },
        IgnoreArg::Yes => quote! { ::skuld::Ignore::Yes },
        IgnoreArg::WithReason(reason) => quote! { ::skuld::Ignore::WithReason(#reason) },
    };
    let req_exprs: Vec<_> = args
        .requires
        .iter()
        .map(|path| {
            let name_str = quote!(#path).to_string();
            quote! { ::skuld::Requirement { name: #name_str, check: #path } }
        })
        .collect();

    // Collect #[fixture] / #[fixture(name)] parameters.
    let mut fixture_params: Vec<FixtureParam> = Vec::new();
    let mut clean_params = Vec::new();

    for param in &func.sig.inputs {
        if let FnArg::Typed(pat_type) = param {
            let fixture_attr = pat_type.attrs.iter().find(|a| a.path().is_ident("fixture"));
            if let Some(attr) = fixture_attr {
                fixture_params.push(parse_fixture_param(attr, pat_type));

                let mut clean = pat_type.clone();
                clean.attrs.retain(|a| !a.path().is_ident("fixture"));
                clean_params.push(FnArg::Typed(clean));
            } else {
                clean_params.push(param.clone());
            }
        } else {
            clean_params.push(param.clone());
        }
    }

    // Fixture names for TestDef.fixture_names.
    let fixture_name_strs: Vec<&str> = fixture_params.iter().map(|p| p.fixture_name.as_str()).collect();

    // Build fixture injection code using fixture_get().
    // Each block references the fixture function as an identifier, which:
    // 1. Forces the crate containing the fixture to be linked (for inventory discovery)
    // 2. Gives a compile-time error if the fixture function is not in scope
    let fixture_setup: Vec<_> = fixture_params
        .iter()
        .enumerate()
        .map(|(i, fp)| {
            let handle_name = format_ident!("__fixture_handle_{}", i);
            let fixture_ident = format_ident!("{}", fp.fixture_name);
            let binding = &fp.binding;
            let target_ty = &fp.target_ty;
            let param_ty = &fp.param_ty;
            let fixture_name = &fp.fixture_name;
            quote! {
                let _ = &#fixture_ident;
                let #handle_name = ::skuld::fixture_get(
                    #fixture_name,
                    ::std::any::TypeId::of::<#target_ty>(),
                );
                let #binding: #param_ty = unsafe { #handle_name.as_ref::<#target_ty>() };
            }
        })
        .collect();

    let vis = &func.vis;
    let block = &func.block;
    let ret = &func.sig.output;
    let fn_token = &func.sig.fn_token;
    let asyncness = &func.sig.asyncness;
    let attrs: Vec<_> = func
        .attrs
        .iter()
        .filter(|a| !a.path().is_ident("ignore") && !a.path().is_ident("should_panic"))
        .collect();
    let call_args: Vec<_> = fixture_params.iter().map(|fp| &fp.binding).collect();

    let is_async = func.sig.asyncness.is_some();
    let await_suffix = if is_async { quote!(.await) } else { quote!() };

    // The core test body: enter scope, inject fixtures, call the test function.
    // IntoTestResult handles both `()` and `Result<(), E>` return types.
    let inner_body_core = if fixture_params.is_empty() {
        quote! {
            let __scope = ::skuld::enter_test_scope(#name_str, ::core::module_path!());
            ::skuld::__private::IntoTestResult::into_test_result(#name()#await_suffix)
        }
    } else {
        quote! {
            let __scope = ::skuld::enter_test_scope(#name_str, ::core::module_path!());
            #(#fixture_setup)*
            ::skuld::__private::IntoTestResult::into_test_result(#name(#(#call_args),*)#await_suffix)
        }
    };

    // For async tests, build the runtime outside catch_unwind (a runtime build
    // failure is an infrastructure error, not a test panic that should satisfy
    // should_panic). The block_on call goes inside catch_unwind.
    let runtime_preamble = if is_async {
        quote! { let __rt = ::skuld::__private::build_async_runtime(); }
    } else {
        quote! {}
    };

    let execute_core = if is_async {
        quote! { __rt.block_on(async { #inner_body_core }) }
    } else {
        quote! { #inner_body_core }
    };

    let body_expr = match &args.should_panic {
        ShouldPanicArg::No => quote! {
            || {
                #runtime_preamble
                #execute_core
            }
        },
        ShouldPanicArg::Yes => quote! {
            || {
                #runtime_preamble
                let __result = ::std::panic::catch_unwind(::std::panic::AssertUnwindSafe(|| {
                    #execute_core
                }));
                if __result.is_ok() {
                    panic!("test did not panic as expected");
                }
            }
        },
        ShouldPanicArg::WithMessage(expected) => quote! {
            || {
                #runtime_preamble
                let __result = ::std::panic::catch_unwind(::std::panic::AssertUnwindSafe(|| {
                    #execute_core
                }));
                match __result {
                    Ok(()) => panic!("test did not panic as expected"),
                    Err(__payload) => {
                        let __msg = if let Some(s) = __payload.downcast_ref::<String>() {
                            s.as_str()
                        } else if let Some(s) = __payload.downcast_ref::<&str>() {
                            *s
                        } else {
                            panic!(
                                "test panicked as expected, but the panic payload is not a string \
                                 (expected message containing {:?})",
                                #expected,
                            );
                        };
                        if !__msg.contains(#expected) {
                            panic!(
                                "test panicked as expected, but the message {:?} \
                                 does not contain {:?}",
                                __msg, #expected,
                            );
                        }
                    }
                }
            }
        },
    };

    let should_panic_expr = match &args.should_panic {
        ShouldPanicArg::No => quote! { ::skuld::ShouldPanic::No },
        ShouldPanicArg::Yes => quote! { ::skuld::ShouldPanic::Yes },
        ShouldPanicArg::WithMessage(msg) => quote! { ::skuld::ShouldPanic::WithMessage(#msg) },
    };

    let serial_str = match &args.serial {
        None => String::new(),
        Some(s) => s.clone(),
    };

    let serial_label_checks = if args.serial_labels.is_empty() {
        quote! {}
    } else {
        let label_idents = &args.serial_labels;
        quote! {
            const _: () = {
                #(let _ = #label_idents;)*
            };
        }
    };

    let expanded = quote! {
        #(#attrs)*
        #vis #asyncness #fn_token #name(#(#clean_params),*) #ret #block

        #serial_label_checks

        ::skuld::inventory::submit!(::skuld::TestDef {
            name: #name_str,
            module: ::core::module_path!(),
            display_name: #display_name_expr,
            requires: &[#(#req_exprs),*],
            fixture_names: &[#(#fixture_name_strs),*],
            ignore: #ignore_expr,
            labels: &[#(#label_paths),*],
            labels_explicit: #labels_explicit,
            serial: #serial_str,
            should_panic: #should_panic_expr,
            body: #body_expr,
        });
    };
    expanded.into()
}

// #[skuld::fixture] ===============================================================================

/// Define a fixture from a function.
///
/// The function must return `Result<T, String>`. The fixture name defaults to
/// the function name, overridable with `name = "..."`.
///
/// ```ignore
/// #[skuld::fixture(scope = process, requires = [docker_available])]
/// fn corpus_image() -> Result<CorpusImage, String> { ... }
///
/// #[skuld::fixture(deref)]
/// fn temp_dir(#[fixture(test_name)] name: &str) -> Result<TempDir, String> { ... }
/// ```
#[proc_macro_attribute]
pub fn fixture(attr: TokenStream, item: TokenStream) -> TokenStream {
    let args = parse_macro_input!(attr as FixtureArgs);
    let mut func = parse_macro_input!(item as ItemFn);
    expand_fixture_def(args, &mut func)
}

fn expand_fixture_def(args: FixtureArgs, func: &mut ItemFn) -> TokenStream {
    let req_exprs: Vec<_> = args
        .requires
        .iter()
        .map(|path| {
            let name_str = quote!(#path).to_string();
            quote! { ::skuld::Requirement { name: #name_str, check: #path } }
        })
        .collect();

    // Determine fixture name. If a custom name is provided, we also generate
    // a public const anchor so that `#[fixture(custom_name)]` in tests can
    // reference the identifier and force the containing crate to be linked.
    let has_custom_name = args.name.is_some();
    let fixture_name = args.name.unwrap_or_else(|| func.sig.ident.to_string());

    // Determine scope.
    let scope_expr = match args.scope.as_ref().map(|s| s.to_string()).as_deref() {
        Some("variable") | None => quote! { ::skuld::FixtureScope::Variable },
        Some("test") => quote! { ::skuld::FixtureScope::Test },
        Some("process") => quote! { ::skuld::FixtureScope::Process },
        _ => unreachable!(), // validated in parser
    };

    // Extract the fixture type from the return type: Result<T, String> → T.
    let fixture_ty = match extract_result_ok_type(&func.sig.output) {
        Some(ty) => ty,
        None => {
            return syn::Error::new_spanned(&func.sig.output, "fixture function must return Result<T, String>")
                .to_compile_error()
                .into();
        }
    };

    // Collect #[fixture] / #[fixture(name)] dependency parameters.
    let mut dep_params: Vec<FixtureParam> = Vec::new();
    let mut clean_inputs = syn::punctuated::Punctuated::new();

    for param in &func.sig.inputs {
        if let FnArg::Typed(pat_type) = param {
            let fixture_attr = pat_type.attrs.iter().find(|a| a.path().is_ident("fixture"));
            if let Some(attr) = fixture_attr {
                dep_params.push(parse_fixture_param(attr, pat_type));
            } else {
                clean_inputs.push(param.clone());
            }
        } else {
            clean_inputs.push(param.clone());
        }
    }

    // Dependency names for FixtureDef.deps.
    let dep_name_strs: Vec<&str> = dep_params.iter().map(|p| p.fixture_name.as_str()).collect();

    // Strip fixture params from the function signature.
    func.sig.inputs = clean_inputs;

    // Prepend dependency injection code to the function body.
    if !dep_params.is_empty() {
        let dep_setup: Vec<_> = dep_params
            .iter()
            .enumerate()
            .map(|(i, fp)| {
                let handle_name = format_ident!("__fixture_dep_handle_{}", i);
                let dep_ident = format_ident!("{}", fp.fixture_name);
                let binding = &fp.binding;
                let target_ty = &fp.target_ty;
                let param_ty = &fp.param_ty;
                let dep_name = &fp.fixture_name;
                quote! {
                    let _ = &#dep_ident;
                    let #handle_name = ::skuld::fixture_get(
                        #dep_name,
                        ::std::any::TypeId::of::<#target_ty>(),
                    );
                    let #binding: #param_ty = unsafe { #handle_name.as_ref::<#target_ty>() };
                }
            })
            .collect();

        let original_stmts = &func.block.stmts;
        func.block = syn::parse_quote!({
            #(#dep_setup)*
            #(#original_stmts)*
        });
    }

    let func_name = &func.sig.ident;

    // Generate cast function.
    let cast_fn = if args.deref {
        quote! {
            {
                fn __cast(
                    any: &(dyn ::std::any::Any + Send + Sync),
                    target: ::std::any::TypeId,
                ) -> Option<::skuld::FixtureRef> {
                    let val = any.downcast_ref::<#fixture_ty>()?;
                    if target == ::std::any::TypeId::of::<#fixture_ty>() {
                        Some(::skuld::FixtureRef::from_ref(val))
                    } else if target == ::std::any::TypeId::of::<<#fixture_ty as ::std::ops::Deref>::Target>() {
                        use ::std::ops::Deref;
                        Some(::skuld::FixtureRef::from_ref(val.deref()))
                    } else {
                        None
                    }
                }
                __cast
            }
        }
    } else {
        quote! {
            {
                fn __cast(
                    any: &(dyn ::std::any::Any + Send + Sync),
                    target: ::std::any::TypeId,
                ) -> Option<::skuld::FixtureRef> {
                    let val = any.downcast_ref::<#fixture_ty>()?;
                    if target == ::std::any::TypeId::of::<#fixture_ty>() {
                        Some(::skuld::FixtureRef::from_ref(val))
                    } else {
                        None
                    }
                }
                __cast
            }
        }
    };

    let fixture_ty_str = quote!(#fixture_ty).to_string();
    let serial_str = match &args.serial {
        None => String::new(),
        Some(s) => s.clone(),
    };

    let serial_label_checks = if args.serial_labels.is_empty() {
        quote! {}
    } else {
        let label_idents = &args.serial_labels;
        quote! {
            const _: () = {
                #(let _ = #label_idents;)*
            };
        }
    };

    // When a custom name is used (name = "..."), the fixture name differs from
    // the function name. Generate a public const anchor so that tests using
    // `#[fixture(custom_name)]` can reference the identifier for linkage.
    let anchor = if has_custom_name {
        let anchor_ident = format_ident!("{}", fixture_name);
        quote! {
            #[doc(hidden)]
            #[allow(non_upper_case_globals, dead_code)]
            pub const #anchor_ident: () = ();
        }
    } else {
        quote! {}
    };

    let expanded = quote! {
        #func

        #anchor

        #serial_label_checks

        ::skuld::inventory::submit!(::skuld::FixtureDef {
            name: #fixture_name,
            scope: #scope_expr,
            requires: &[#(#req_exprs),*],
            deps: &[#(#dep_name_strs),*],
            setup: || -> ::core::result::Result<
                ::std::boxed::Box<dyn ::std::any::Any + ::core::marker::Send + ::core::marker::Sync>,
                ::std::string::String,
            > {
                #func_name().map(|v| {
                    ::std::boxed::Box::new(v)
                        as ::std::boxed::Box<dyn ::std::any::Any + ::core::marker::Send + ::core::marker::Sync>
                })
            },
            cast: #cast_fn,
            type_name: #fixture_ty_str,
            serial: #serial_str,
        });
    };
    expanded.into()
}

// #[skuld::label] ================================================================================

/// Parsed form of `$(#[...])* $vis const $ident : $ty ;`. `ItemConst` is not
/// reusable here because it requires an `= expr` initializer.
struct LabelItem {
    attrs: Vec<Attribute>,
    vis: Visibility,
    _const_token: Token![const],
    ident: Ident,
    _colon: Token![:],
    ty: Type,
    _semi: Token![;],
}

impl Parse for LabelItem {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        Ok(Self {
            attrs: input.call(Attribute::parse_outer)?,
            vis: input.parse()?,
            _const_token: input.parse()?,
            ident: input.parse()?,
            _colon: input.parse()?,
            ty: input.parse()?,
            _semi: input.parse()?,
        })
    }
}

/// Declare a label constant. The label's string name is the identifier
/// lowercased (`FOO` → `"foo"`).
///
/// ```ignore
/// #[skuld::label]
/// pub const DOCKER: skuld::Label;
/// ```
///
/// The type position accepts any path resolving to `skuld::Label` — so aliases
/// (`use skuld::Label as L;`) work. A mismatched type surfaces as a normal
/// `E0308: mismatched types` error at the RHS.
///
/// Attributes on the declaration are preserved: doc comments decorate the
/// const; gating attributes like `#[cfg(test)]` are forwarded to the
/// inventory submission so the const and its registration stay in lock-step.
#[proc_macro_attribute]
pub fn label(attr: TokenStream, item: TokenStream) -> TokenStream {
    parse_macro_input!(attr as Nothing);
    let LabelItem {
        attrs, vis, ident, ty, ..
    } = parse_macro_input!(item as LabelItem);
    let name_str = ident.to_string().to_ascii_lowercase();
    // Only gating attributes (`cfg`, `cfg_attr`) are forwarded to the
    // `inventory::submit!` invocation so the const and its registration stay
    // in lock-step. Everything else (doc, allow, deprecated, must_use, ...)
    // applies only to the const; attaching `#[deprecated]` to a macro
    // invocation would be a compile error.
    let submit_attrs: Vec<&Attribute> = attrs
        .iter()
        .filter(|a| a.path().is_ident("cfg") || a.path().is_ident("cfg_attr"))
        .collect();
    let expanded = quote! {
        #(#attrs)*
        #vis const #ident: #ty = ::skuld::Label::__new(#name_str);

        #(#submit_attrs)*
        ::skuld::inventory::submit! {
            ::skuld::LabelEntry {
                name: #name_str,
                file: ::core::file!(),
                line: ::core::line!(),
                column: ::core::column!(),
            }
        }
    };
    expanded.into()
}

// Helpers =====================================================================================

/// Build the `display_name: Option<&'static str>` expression.
fn build_display_name(custom_name: &Option<String>) -> proc_macro2::TokenStream {
    match custom_name {
        Some(n) => quote! { Some(#n) },
        None => quote! { None },
    }
}

/// Strip a leading `&` from a type (e.g. `&TempDir` → `TempDir`).
fn strip_reference(ty: &Type) -> Type {
    if let Type::Reference(r) = ty {
        (*r.elem).clone()
    } else {
        ty.clone()
    }
}

/// Extract `T` from a return type of `Result<T, String>` or `Result<T, E>`.
fn extract_result_ok_type(ret: &ReturnType) -> Option<Type> {
    let ReturnType::Type(_, ty) = ret else {
        return None;
    };
    let Type::Path(type_path) = ty.as_ref() else {
        return None;
    };
    let last_seg = type_path.path.segments.last()?;
    if last_seg.ident != "Result" {
        return None;
    }
    let syn::PathArguments::AngleBracketed(args) = &last_seg.arguments else {
        return None;
    };
    let first_arg = args.args.first()?;
    let syn::GenericArgument::Type(ok_ty) = first_arg else {
        return None;
    };
    Some(ok_ty.clone())
}
