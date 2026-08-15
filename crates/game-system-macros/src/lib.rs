//! Procedural macro for project-local Rust game systems.

mod each;
mod regular;

use proc_macro::TokenStream;
use quote::{format_ident, quote};
use syn::parse::Parser;
use syn::{parse_macro_input, Expr, ItemFn, Lit, Meta, Token};

/// Registers a query-scoped project-local Rust system.
///
/// Add the `each` flag to derive one entity query from callback parameters and
/// invoke the callback once for every matching entity.
#[proc_macro_attribute]
pub fn game_system(args: TokenStream, item: TokenStream) -> TokenStream {
    match expand(args.into(), parse_macro_input!(item as ItemFn)) {
        Ok(tokens) => tokens.into(),
        Err(error) => error.to_compile_error().into(),
    }
}

pub(crate) struct SystemConfig {
    pub(crate) each: bool,
    pub(crate) id: String,
    pub(crate) display_name: String,
    pub(crate) description: String,
    pub(crate) schedule: proc_macro2::TokenStream,
    pub(crate) order: i32,
    pub(crate) before: Vec<String>,
    pub(crate) after: Vec<String>,
    pub(crate) aliases: Vec<String>,
}

fn expand(
    args: proc_macro2::TokenStream,
    function: ItemFn,
) -> syn::Result<proc_macro2::TokenStream> {
    let config = parse_config(args, &function)?;
    if config.each {
        each::expand(function, config)
    } else {
        regular::expand(function, config)
    }
}

fn parse_config(args: proc_macro2::TokenStream, function: &ItemFn) -> syn::Result<SystemConfig> {
    let parser = syn::punctuated::Punctuated::<Meta, Token![,]>::parse_terminated;
    let metas = parser.parse2(args)?;
    let mut each = false;
    let mut schedule = None;
    let mut order = 0;
    let mut id = None;
    let mut display_name = None;
    let mut description = None;
    let mut before = Vec::new();
    let mut after = Vec::new();
    let mut aliases = Vec::new();

    for meta in metas {
        match meta {
            Meta::Path(path) if path.is_ident("each") => {
                if std::mem::replace(&mut each, true) {
                    return Err(syn::Error::new_spanned(path, "duplicate `each` flag"));
                }
            }
            Meta::NameValue(value) => {
                if value.path.is_ident("schedule") {
                    schedule = Some(string_expr(&value.value, "schedule")?);
                } else if value.path.is_ident("order") {
                    let Expr::Lit(expr) = value.value else {
                        return Err(syn::Error::new_spanned(value, "order must be an integer"));
                    };
                    let Lit::Int(lit) = expr.lit else {
                        return Err(syn::Error::new_spanned(expr, "order must be an integer"));
                    };
                    order = lit.base10_parse()?;
                } else if value.path.is_ident("id") {
                    id = Some(string_expr(&value.value, "id")?);
                } else if value.path.is_ident("display_name") {
                    display_name = Some(string_expr(&value.value, "display_name")?);
                } else if value.path.is_ident("description") {
                    description = Some(string_expr(&value.value, "description")?);
                } else if value.path.is_ident("before") {
                    before = string_array(&value.value, "before")?;
                } else if value.path.is_ident("after") {
                    after = string_array(&value.value, "after")?;
                } else if value.path.is_ident("aliases") {
                    aliases = string_array(&value.value, "aliases")?;
                } else {
                    return Err(syn::Error::new_spanned(
                        value.path,
                        "supported keys are each, id, display_name, description, schedule, order, before, after, and aliases",
                    ));
                }
            }
            other => {
                return Err(syn::Error::new_spanned(
                    other,
                    "expected `each` or `name = value`",
                ));
            }
        }
    }

    let schedule = match schedule.as_deref().unwrap_or("update") {
        "update" => quote!(engine::game_module::GameSystemSchedule::Update),
        "fixed_update" => quote!(engine::game_module::GameSystemSchedule::FixedUpdate),
        other => {
            return Err(syn::Error::new_spanned(
                &function.sig.ident,
                format!("unsupported schedule `{other}`"),
            ));
        }
    };
    let rust_name = function.sig.ident.to_string();
    let id = id.unwrap_or_else(|| format!("game.{rust_name}"));
    let display_name = display_name.unwrap_or_else(|| split_identifier(&rust_name));
    let description =
        description.unwrap_or_else(|| format!("Project-local Rust system `{display_name}`."));

    Ok(SystemConfig {
        each,
        id,
        display_name,
        description,
        schedule,
        order,
        before,
        after,
        aliases,
    })
}

pub(crate) fn register(
    function: ItemFn,
    adapter: proc_macro2::TokenStream,
    access: syn::Ident,
    invoke: syn::Ident,
    config: SystemConfig,
) -> proc_macro2::TokenStream {
    let name = &function.sig.ident;
    let wrapper = format_ident!("__iroha_run_{}", name);
    let SystemConfig {
        id,
        display_name,
        description,
        schedule,
        order,
        before,
        after,
        aliases,
        ..
    } = config;
    quote! {
        #function
        #adapter

        #[doc(hidden)]
        unsafe extern "C" fn #wrapper(
            input_json: *const u8,
            input_json_len: usize,
            output: *mut engine::game_module::GameBufferAbi,
            error_buffer: *mut u8,
            error_buffer_len: usize,
        ) -> bool {
            // SAFETY: The host supplies readable invocation JSON and writable
            // output/error descriptors after validating the module ABI.
            unsafe {
                engine::game_module::run_typed_system_ffi(
                    input_json,
                    input_json_len,
                    output,
                    error_buffer,
                    error_buffer_len,
                    #invoke,
                )
            }
        }

        engine::inventory::submit! {
            engine::game_module::GameSystemRegistration {
                id: #id,
                name: stringify!(#name),
                display_name: #display_name,
                description: #description,
                schedule: #schedule,
                order: #order,
                before: &[#(#before),*],
                after: &[#(#after),*],
                aliases: &[#(#aliases),*],
                access: #access,
                run: #wrapper,
            }
        }
    }
}

fn string_expr(expression: &Expr, label: &str) -> syn::Result<String> {
    let Expr::Lit(expression) = expression else {
        return Err(syn::Error::new_spanned(
            expression,
            format!("{label} must be a string"),
        ));
    };
    let Lit::Str(value) = &expression.lit else {
        return Err(syn::Error::new_spanned(
            expression,
            format!("{label} must be a string"),
        ));
    };
    Ok(value.value())
}

fn string_array(expression: &Expr, label: &str) -> syn::Result<Vec<String>> {
    let Expr::Array(array) = expression else {
        return Err(syn::Error::new_spanned(
            expression,
            format!("{label} must be an array of strings"),
        ));
    };
    array
        .elems
        .iter()
        .map(|value| string_expr(value, label))
        .collect()
}

fn split_identifier(identifier: &str) -> String {
    let mut words = Vec::new();
    let mut current = String::new();
    for character in identifier.chars() {
        if character == '_' || (character.is_uppercase() && !current.is_empty()) {
            words.push(current);
            current = String::new();
        }
        if current.is_empty() {
            current.extend(character.to_uppercase());
        } else {
            current.push(character);
        }
    }
    if !current.is_empty() {
        words.push(current);
    }
    words.join(" ")
}

#[cfg(test)]
mod tests {
    use super::expand;
    use quote::quote;
    use syn::ItemFn;

    fn expanded(args: proc_macro2::TokenStream, item: proc_macro2::TokenStream) -> String {
        let item = syn::parse2::<ItemFn>(item).expect("test function parses");
        expand(args, item).expect("macro expands").to_string()
    }

    #[test]
    fn regular_system_uses_existing_system_parameters() {
        let output = expanded(
            quote!(),
            quote!(
                fn tick(time: Time, commands: Commands) {}
            ),
        );
        assert!(output.contains("< Time as engine :: game_api :: GameSystemParam > :: declare"));
        assert!(output.contains("< Commands as engine :: game_api :: GameSystemParam > :: declare"));
        assert!(!output.contains("__IrohaEachQuery"));
        assert!(!output.contains("__IrohaQuerySpec_tick_0"));
    }

    #[test]
    fn regular_system_derives_tuple_query_access_and_decoder() {
        let output = expanded(
            quote!(),
            quote! {
                fn move_system(entities: Query<(&MoveRule, &Transform)>) {
                    for (_rule, _transform) in entities.iter() {}
                }
            },
        );
        assert!(output.contains("game.move_system.query.0"));
        assert!(output.contains("read :: < MoveRule >"));
        assert!(output.contains("LocalTransformView"));
        assert!(output.contains("component :: < MoveRule >"));
        assert!(output.contains("Transform :: from"));
        assert!(output.contains("__IrohaQuery_move_system_0"));
    }

    #[test]
    fn regular_system_derives_distinct_query_ids_and_without_filter() {
        let output = expanded(
            quote!(id = "game.targeting"),
            quote! {
                fn targeting_system(
                    attackers: Query<(&Attacker, &Transform)>,
                    targets: Query<(&Target, &Transform), Without<Dead>>,
                ) {
                    let _ = attackers.iter().count();
                    let _ = targets.iter().count();
                }
            },
        );
        assert!(output.contains("game.targeting.query.0"));
        assert!(output.contains("game.targeting.query.1"));
        assert!(output.contains("optional :: < Dead >"));
        assert!(output.contains("optional_component :: < Dead >"));
        assert!(output.contains("continue"));
    }

    #[test]
    fn regular_system_derives_writable_query_flush() {
        let output = expanded(
            quote!(),
            quote! {
                fn update_health(
                    entities: Query<(&mut Health, Option<&mut Buff>, &mut Transform)>,
                ) {
                    for (_health, _buff, _transform) in entities.iter() {}
                }
            },
        );
        assert!(output.contains("write :: < Health >"));
        assert!(output.contains("optional_write :: < Buff >"));
        assert!(output.contains("GameCommandFamily :: Transform"));
        assert!(output.contains("__iroha_query . set"));
        assert!(output.contains("set_transform"));
        assert!(output.contains("__iroha_flush"));
    }

    #[test]
    fn each_system_derives_component_and_transform_access() {
        let output = expanded(
            quote!(each),
            quote! {
                fn move_each(
                    rule: &MoveRule,
                    boost: Option<&SpeedBoost>,
                    health: &mut Health,
                    transform: &mut Transform,
                ) {}
            },
        );
        assert!(output.contains("read :: < MoveRule >"));
        assert!(output.contains("optional :: < SpeedBoost >"));
        assert!(output.contains("write :: < Health >"));
        assert!(output.contains("LocalTransformView"));
        assert!(output.contains("set_transform"));
    }

    #[test]
    fn each_system_derives_or_and_exclusion_filters() {
        let output = expanded(
            quote!(each),
            quote! {
                fn characters(
                    role: AnyOf<(&Player, &Enemy)>,
                    alive: Without<Dead>,
                    entity: Entity,
                ) {}
            },
        );
        assert!(output.contains("optional :: < Player >"));
        assert!(output.contains("optional :: < Enemy >"));
        assert!(output.contains("optional :: < Dead >"));
        assert!(output.contains("is_empty"));
        assert!(output.contains("continue"));
    }
}
