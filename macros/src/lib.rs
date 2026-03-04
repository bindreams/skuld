use proc_macro::TokenStream;
use quote::{format_ident, quote};
use syn::parse::{Parse, ParseStream};
use syn::{
    bracketed, parse_macro_input, punctuated::Punctuated, FnArg, Ident, ItemFn, LitStr, Path, ReturnType, Token, Type,
};

// #[skuld::test] argument parsing =================================================================

/// Parsed arguments for `#[skuld::test(...)]`.
#[derive(Default)]
struct TestArgs {
    requires: Vec<Path>,
    name: Option<String>,
    labels: Option<Vec<Ident>>,
    ignore: IgnoreArg,
    serial: bool,
}

#[derive(Default)]
enum IgnoreArg {
    #[default]
    No,
    Yes,
    WithReason(String),
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
                        Punctuated::<Ident, Token![,]>::parse_terminated(&content)?
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
                    args.serial = true;
                }
                other => {
                    return Err(syn::Error::new(
                        key.span(),
                        format!("unknown argument `{other}`; expected requires, name, labels, ignore, or serial"),
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

// #[skuld::fixture] argument parsing ==============================================================

/// Parsed arguments for `#[skuld::fixture(...)]`.
#[derive(Default)]
struct FixtureArgs {
    requires: Vec<Path>,
    scope: Option<Ident>,
    name: Option<String>,
    deref: bool,
    serial: bool,
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
                    args.serial = true;
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
/// ```ignore
/// #[skuld::test(requires = [preconditions::valgrind], labels = [slow])]
/// fn my_test(#[fixture(temp_dir)] dir: &Path) { /* ... */ }
/// ```
#[proc_macro_attribute]
pub fn test(attr: TokenStream, item: TokenStream) -> TokenStream {
    let args = parse_macro_input!(attr as TestArgs);
    let func = parse_macro_input!(item as ItemFn);
    expand_test_def(args, func)
}

fn expand_test_def(args: TestArgs, func: ItemFn) -> TokenStream {
    // Guard: reject #[test] on the same function.
    for attr in &func.attrs {
        if attr.path().is_ident("test") {
            return syn::Error::new_spanned(attr, "remove #[test] — #[skuld::test] already registers this function")
                .to_compile_error()
                .into();
        }
    }

    let name = &func.sig.ident;
    let name_str = name.to_string();

    let display_name_expr = build_display_name(&args.name);
    let labels_explicit = args.labels.is_some();
    let label_strs: Vec<String> = args
        .labels
        .unwrap_or_default()
        .iter()
        .map(|id| id.to_string())
        .collect();
    let ignore_expr = match &args.ignore {
        IgnoreArg::No => quote! { ::skuld::Ignore::No },
        IgnoreArg::Yes => quote! { ::skuld::Ignore::Yes },
        IgnoreArg::WithReason(reason) => quote! { ::skuld::Ignore::WithReason(#reason) },
    };
    let req_exprs: Vec<_> = args.requires.iter().collect();

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
    let fixture_setup: Vec<_> = fixture_params
        .iter()
        .enumerate()
        .map(|(i, fp)| {
            let handle_name = format_ident!("__fixture_handle_{}", i);
            let binding = &fp.binding;
            let target_ty = &fp.target_ty;
            let param_ty = &fp.param_ty;
            let fixture_name = &fp.fixture_name;
            quote! {
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
    let attrs: Vec<_> = func.attrs.iter().collect();
    let call_args: Vec<_> = fixture_params.iter().map(|fp| &fp.binding).collect();

    let body_expr = if fixture_params.is_empty() {
        quote! {
            || {
                let __scope = ::skuld::enter_test_scope(#name_str, ::core::module_path!());
                #name();
            }
        }
    } else {
        quote! {
            || {
                let __scope = ::skuld::enter_test_scope(#name_str, ::core::module_path!());
                #(#fixture_setup)*
                #name(#(#call_args),*);
            }
        }
    };

    let serial = args.serial;

    let expanded = quote! {
        #(#attrs)*
        #vis #fn_token #name(#(#clean_params),*) #ret #block

        ::skuld::inventory::submit!(::skuld::TestDef {
            name: #name_str,
            module: ::core::module_path!(),
            display_name: #display_name_expr,
            requires: &[#(#req_exprs),*],
            fixture_names: &[#(#fixture_name_strs),*],
            ignore: #ignore_expr,
            labels: &[#(#label_strs),*],
            labels_explicit: #labels_explicit,
            serial: #serial,
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
    let req_exprs: Vec<_> = args.requires.iter().collect();

    // Determine fixture name.
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
                let binding = &fp.binding;
                let target_ty = &fp.target_ty;
                let param_ty = &fp.param_ty;
                let dep_name = &fp.fixture_name;
                quote! {
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
    let serial = args.serial;

    let expanded = quote! {
        #func

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
            serial: #serial,
        });
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
