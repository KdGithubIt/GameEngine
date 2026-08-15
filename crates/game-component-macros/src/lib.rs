//! Explicit authoring-field derive for project-local Rust components.

use proc_macro::TokenStream;
use quote::{format_ident, quote};
use syn::{parse_macro_input, Attribute, Data, DeriveInput, Fields, Ident, Meta, Type};

/// Derives the runtime and authoring contracts for a project component.
///
/// Only fields marked with bare `#[game_field]` are exposed in the Inspector and
/// persisted in scenes or prefabs. Unmarked fields are runtime-only and are
/// restored from `Default` whenever authoring data is decoded.
#[proc_macro_derive(GameComponent, attributes(game_component, game_field))]
pub fn derive_game_component(input: TokenStream) -> TokenStream {
    match derive_game_component_impl(parse_macro_input!(input as DeriveInput)) {
        Ok(tokens) => tokens.into(),
        Err(error) => error.to_compile_error().into(),
    }
}

#[derive(Clone)]
struct AuthoredField {
    ident: Ident,
    ty: Type,
    conditional_attrs: Vec<Attribute>,
}

fn derive_game_component_impl(input: DeriveInput) -> syn::Result<proc_macro2::TokenStream> {
    if !input.generics.params.is_empty() || input.generics.where_clause.is_some() {
        return Err(syn::Error::new_spanned(
            &input.generics,
            "GameComponent does not support generic component types",
        ));
    }

    let name = input.ident;
    let helper_module = format_ident!("__iroha_game_component_{}", name);
    let component_attrs = input
        .attrs
        .iter()
        .filter(|attribute| attribute.path().is_ident("game_component"))
        .cloned()
        .collect::<Vec<_>>();
    let fields = match input.data {
        Data::Struct(data) => match data.fields {
            Fields::Named(fields) => fields.named,
            _ => {
                return Err(syn::Error::new_spanned(
                    name,
                    "GameComponent requires a struct with named fields",
                ))
            }
        },
        _ => {
            return Err(syn::Error::new_spanned(
                name,
                "GameComponent can only be derived for structs",
            ))
        }
    };

    let mut authored_fields = Vec::new();
    for field in fields {
        if game_component_field_is_authored(&field)? {
            authored_fields.push(AuthoredField {
                ident: field.ident.expect("named fields have identifiers"),
                ty: field.ty,
                conditional_attrs: field
                    .attrs
                    .into_iter()
                    .filter(|attribute| {
                        attribute.path().is_ident("cfg")
                            || attribute.path().is_ident("cfg_attr")
                    })
                    .collect(),
            });
        }
    }

    let helper_fields = authored_fields.iter().map(|field| {
        let attrs = &field.conditional_attrs;
        let ident = &field.ident;
        let ty = &field.ty;
        quote! {
            #(#attrs)*
            pub(super) #ident: #ty
        }
    });
    let helper_defaults = authored_fields.iter().map(|field| {
        let attrs = &field.conditional_attrs;
        let ident = &field.ident;
        quote! {
            #(#attrs)*
            #ident: source.#ident
        }
    });
    let decode_assignments = authored_fields.iter().map(|field| {
        let attrs = &field.conditional_attrs;
        let ident = &field.ident;
        quote! {
            #(#attrs)*
            {
                component.#ident = authored.#ident;
            }
        }
    });
    let encode_fields = authored_fields.iter().map(|field| {
        let attrs = &field.conditional_attrs;
        let ident = &field.ident;
        let ty = &field.ty;
        let field_name = ident.to_string();
        quote! {
            #(#attrs)*
            {
                fields.insert(
                    #field_name.to_owned(),
                    <#ty as engine::game_module::GameField>::to_value(&self.#ident),
                );
            }
        }
    });

    let helper_default = if authored_fields.is_empty() {
        quote! {
            Self {}
        }
    } else {
        quote! {
            #[allow(unused_variables)]
            let source = super::#name::default();
            Self {
                #(#helper_defaults),*
            }
        }
    };
    let decode = if authored_fields.is_empty() {
        quote! {
            let _ =
                <#helper_module::#name as engine::game_module::GameComponent>::from_authoring_value(value)?;
            Ok(Self::default())
        }
    } else {
        quote! {
            let authored =
                <#helper_module::#name as engine::game_module::GameComponent>::from_authoring_value(value)?;
            let mut component = Self::default();
            #(#decode_assignments)*
            Ok(component)
        }
    };
    let encode = if authored_fields.is_empty() {
        quote! {
            engine::game_module::Value::Object(::std::collections::BTreeMap::new())
        }
    } else {
        quote! {
            #[allow(unused_mut)]
            let mut fields = ::std::collections::BTreeMap::new();
            #(#encode_fields)*
            engine::game_module::Value::Object(fields)
        }
    };

    Ok(quote! {
        #[doc(hidden)]
        #[allow(non_snake_case)]
        mod #helper_module {
            #[derive(engine::__LegacyGameComponent)]
            #(#component_attrs)*
            pub(super) struct #name {
                #(#helper_fields),*
            }

            impl ::std::default::Default for #name {
                fn default() -> Self {
                    #helper_default
                }
            }
        }

        impl engine::game_module::GameComponent for #name {
            const TYPE_ID: &'static str =
                <#helper_module::#name as engine::game_module::GameComponent>::TYPE_ID;
            const DISPLAY_NAME: &'static str =
                <#helper_module::#name as engine::game_module::GameComponent>::DISPLAY_NAME;

            fn schema() -> engine::game_module::ComponentSchema {
                <#helper_module::#name as engine::game_module::GameComponent>::schema()
            }

            fn from_authoring_value(
                value: &engine::game_module::Value,
            ) -> Result<Self, String> {
                #decode
            }

            fn to_authoring_value(&self) -> engine::game_module::Value {
                #encode
            }
        }
    })
}

fn game_component_field_is_authored(field: &syn::Field) -> syn::Result<bool> {
    let mut attributes = field
        .attrs
        .iter()
        .filter(|attribute| attribute.path().is_ident("game_field"));
    let Some(attribute) = attributes.next() else {
        return Ok(false);
    };
    if let Some(duplicate) = attributes.next() {
        return Err(syn::Error::new_spanned(
            duplicate,
            "duplicate `game_field` attribute",
        ));
    }

    match &attribute.meta {
        Meta::Path(_) => Ok(true),
        Meta::List(list) if list.tokens.is_empty() => Err(syn::Error::new_spanned(
            attribute,
            "use bare `#[game_field]`; empty parentheses are not supported",
        )),
        Meta::List(_) => Err(syn::Error::new_spanned(
            attribute,
            "`#[game_field]` does not accept options; remove `(skip)` from runtime-only fields",
        )),
        Meta::NameValue(_) => Err(syn::Error::new_spanned(
            attribute,
            "use bare `#[game_field]` without a value",
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::{derive_game_component_impl, game_component_field_is_authored};
    use quote::quote;
    use syn::parse::Parser;

    fn parse_named_field(tokens: proc_macro2::TokenStream) -> syn::Field {
        syn::Field::parse_named.parse2(tokens).unwrap()
    }

    #[test]
    fn unmarked_fields_are_runtime_only() {
        let field = parse_named_field(quote!(runtime_cache: RuntimeCache));

        assert!(!game_component_field_is_authored(&field).unwrap());
    }

    #[test]
    fn bare_game_field_is_authored() {
        let field = parse_named_field(quote!(
            #[game_field]
            speed: f64
        ));

        assert!(game_component_field_is_authored(&field).unwrap());
    }

    #[test]
    fn skip_option_is_rejected() {
        let field = parse_named_field(quote!(
            #[game_field(skip)]
            runtime_cache: RuntimeCache
        ));

        let error = game_component_field_is_authored(&field).unwrap_err();
        assert!(error.to_string().contains("remove `(skip)`"));
    }

    #[test]
    fn empty_game_field_parentheses_are_rejected() {
        let field = parse_named_field(quote!(
            #[game_field()]
            speed: f64
        ));

        let error = game_component_field_is_authored(&field).unwrap_err();
        assert!(error.to_string().contains("bare `#[game_field]`"));
    }

    #[test]
    fn duplicate_game_field_attributes_are_rejected() {
        let field = parse_named_field(quote!(
            #[game_field]
            #[game_field]
            speed: f64
        ));

        let error = game_component_field_is_authored(&field).unwrap_err();
        assert!(error.to_string().contains("duplicate `game_field`"));
    }

    #[test]
    fn expansion_serializes_only_explicit_fields() {
        let input = syn::parse2(quote! {
            #[game_component(display_name = "Example")]
            struct Example {
                #[game_field]
                speed: f64,
                runtime_cache: RuntimeCache,
            }
        })
        .unwrap();

        let expanded = derive_game_component_impl(input).unwrap().to_string();
        assert!(expanded.contains("__LegacyGameComponent"));
        assert!(expanded.contains("speed"));
        assert!(!expanded.contains("runtime_cache"));
    }
}
