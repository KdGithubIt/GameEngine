//! Procedural macros used by project-local Rust game modules.

use proc_macro::TokenStream;
use quote::{format_ident, quote};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use syn::parse::Parser;
use syn::{
    parse_macro_input, Attribute, DeriveInput, Expr, Fields, FnArg, Item, ItemFn, Lit, Meta,
    ReturnType, Token, Type,
};

/// Derives a stable Project Settings input-action marker.
#[proc_macro_derive(InputAction, attributes(input_action))]
pub fn derive_input_action(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    match marker_string_attribute(&input, "input_action", "name") {
        Ok(value) => {
            let name = input.ident;
            quote! {
                impl engine::game_api::InputAction for #name {
                    const NAME: &'static str = #value;
                }
            }
            .into()
        }
        Err(error) => error.to_compile_error().into(),
    }
}

/// Derives a typed active-save key marker.
#[proc_macro_derive(SaveKey, attributes(save_key))]
pub fn derive_save_key(input: TokenStream) -> TokenStream {
    match derive_save_key_impl(parse_macro_input!(input as DeriveInput)) {
        Ok(tokens) => tokens.into(),
        Err(error) => error.to_compile_error().into(),
    }
}

fn derive_save_key_impl(input: DeriveInput) -> syn::Result<proc_macro2::TokenStream> {
    let name = input.ident.clone();
    ensure_unit_struct(&input, "SaveKey")?;
    let mut key = None;
    let mut value_type = None;
    for attribute in input
        .attrs
        .iter()
        .filter(|attribute| attribute.path().is_ident("save_key"))
    {
        attribute.parse_nested_meta(|meta| {
            if meta.path.is_ident("name") {
                key = Some(meta.value()?.parse::<syn::LitStr>()?.value());
            } else if meta.path.is_ident("value") {
                value_type = Some(meta.value()?.parse::<Type>()?);
            } else {
                return Err(meta.error("supported keys are name and value"));
            }
            Ok(())
        })?;
    }
    let key = key.ok_or_else(|| {
        syn::Error::new_spanned(
            &name,
            "missing #[save_key(name = \"...\", value = Type)] attribute",
        )
    })?;
    let value_type = value_type
        .ok_or_else(|| syn::Error::new_spanned(&name, "save_key requires a value type"))?;
    Ok(quote! {
        impl engine::game_api::SaveKey for #name {
            type Value = #value_type;
            const NAME: &'static str = #key;
        }
    })
}

/// Derives a type-safe entity-query access specification.
#[proc_macro_derive(GameQuerySpec, attributes(game_query))]
pub fn derive_game_query_spec(input: TokenStream) -> TokenStream {
    match derive_game_query_spec_impl(parse_macro_input!(input as DeriveInput)) {
        Ok(tokens) => tokens.into(),
        Err(error) => error.to_compile_error().into(),
    }
}

fn derive_game_query_spec_impl(input: DeriveInput) -> syn::Result<proc_macro2::TokenStream> {
    let name = input.ident.clone();
    ensure_unit_struct(&input, "GameQuerySpec")?;
    let mut id = None;
    let mut reads = Vec::<Type>::new();
    let mut optional = Vec::<Type>::new();
    let mut writes = Vec::<Type>::new();
    let mut optional_writes = Vec::<Type>::new();
    let mut views = Vec::<Type>::new();
    let mut optional_views = Vec::<Type>::new();
    for attribute in input
        .attrs
        .iter()
        .filter(|attribute| attribute.path().is_ident("game_query"))
    {
        attribute.parse_nested_meta(|meta| {
            if meta.path.is_ident("id") {
                id = Some(meta.value()?.parse::<syn::LitStr>()?.value());
                return Ok(());
            }
            let target = if meta.path.is_ident("read") {
                &mut reads
            } else if meta.path.is_ident("optional") {
                &mut optional
            } else if meta.path.is_ident("write") {
                &mut writes
            } else if meta.path.is_ident("optional_write") {
                &mut optional_writes
            } else if meta.path.is_ident("view") {
                &mut views
            } else if meta.path.is_ident("optional_view") {
                &mut optional_views
            } else {
                return Err(meta.error("supported keys are id, read, optional, write, optional_write, view, and optional_view"));
            };
            target.push(meta.value()?.parse::<Type>()?);
            Ok(())
        })?;
    }
    let id = id.ok_or_else(|| {
        syn::Error::new_spanned(
            &name,
            "missing #[game_query(id = \"game.query...\")] attribute",
        )
    })?;
    Ok(quote! {
        impl engine::game_api::QuerySpec for #name {
            const ID: &'static str = #id;
            fn access() -> engine::game_io::GameQueryAccess {
                engine::game_api::QueryAccessBuilder::new(Self::ID)
                    #(.read::<#reads>())*
                    #(.optional::<#optional>())*
                    #(.write::<#writes>())*
                    #(.optional_write::<#optional_writes>())*
                    #(.view::<#views>())*
                    #(.optional_view::<#optional_views>())*
                    .build()
            }
        }
    })
}

fn marker_string_attribute(
    input: &DeriveInput,
    attribute_name: &str,
    key: &str,
) -> syn::Result<String> {
    ensure_unit_struct(input, attribute_name)?;
    let mut value = None;
    for attribute in input
        .attrs
        .iter()
        .filter(|attribute| attribute.path().is_ident(attribute_name))
    {
        attribute.parse_nested_meta(|meta| {
            if meta.path.is_ident(key) {
                value = Some(meta.value()?.parse::<syn::LitStr>()?.value());
                Ok(())
            } else {
                Err(meta.error(format!("supported key is {key}")))
            }
        })?;
    }
    value.ok_or_else(|| {
        syn::Error::new_spanned(
            &input.ident,
            format!("missing #[{attribute_name}({key} = \"...\")] attribute"),
        )
    })
}

fn ensure_unit_struct(input: &DeriveInput, derive_name: &str) -> syn::Result<()> {
    match &input.data {
        syn::Data::Struct(data) if matches!(data.fields, Fields::Unit) => Ok(()),
        _ => Err(syn::Error::new_spanned(
            &input.ident,
            format!("{derive_name} requires a unit struct"),
        )),
    }
}

/// Derives Iroha's runtime spawn and authoring schema contracts for a component.
///
/// The input type must implement `Default`, `Send`, and `Sync`. Every named
/// field not marked `#[game_field(skip)]` must implement
/// `engine::game_module::GameField`. Skipped fields are runtime-only and are
/// restored from `Default` whenever authoring data is decoded.
#[proc_macro_derive(GameComponent, attributes(game_component, game_field))]
pub fn derive_game_component(input: TokenStream) -> TokenStream {
    match derive_game_component_impl(parse_macro_input!(input as DeriveInput)) {
        Ok(tokens) => tokens.into(),
        Err(error) => error.to_compile_error().into(),
    }
}

/// Derives Iroha's runtime-only project resource schema contract.
///
/// Resources hold project-wide Play state owned by the host. Unlike
/// [`GameComponent`], the derived type is not inserted into the ECS and is
/// never persisted unless gameplay explicitly copies a value into save data.
#[proc_macro_derive(GameResource, attributes(game_resource))]
pub fn derive_game_resource(input: TokenStream) -> TokenStream {
    match derive_game_resource_impl(parse_macro_input!(input as DeriveInput)) {
        Ok(tokens) => tokens.into(),
        Err(error) => error.to_compile_error().into(),
    }
}

fn derive_game_resource_impl(input: DeriveInput) -> syn::Result<proc_macro2::TokenStream> {
    let name = input.ident;
    let mut resource_id = None;
    let mut display_name = None;

    for attribute in input
        .attrs
        .iter()
        .filter(|attr| attr.path().is_ident("game_resource"))
    {
        attribute.parse_nested_meta(|meta| {
            if meta.path.is_ident("id") {
                resource_id = Some(meta.value()?.parse::<syn::LitStr>()?.value());
            } else if meta.path.is_ident("display_name") {
                display_name = Some(meta.value()?.parse::<syn::LitStr>()?.value());
            } else {
                return Err(meta.error("supported keys are id and display_name"));
            }
            Ok(())
        })?;
    }

    let resource_id = resource_id.ok_or_else(|| {
        syn::Error::new_spanned(
            &name,
            "missing #[game_resource(id = \"game...\")] attribute",
        )
    })?;
    let display_name = display_name.unwrap_or_else(|| split_identifier(&name.to_string()));
    let fields = match input.data {
        syn::Data::Struct(data) => match data.fields {
            Fields::Named(fields) => fields.named,
            _ => {
                return Err(syn::Error::new_spanned(
                    name,
                    "GameResource requires a struct with named fields",
                ))
            }
        },
        _ => {
            return Err(syn::Error::new_spanned(
                name,
                "GameResource can only be derived for structs",
            ))
        }
    };

    let field_idents: Vec<_> = fields
        .iter()
        .map(|field| field.ident.clone().expect("named fields have identifiers"))
        .collect();
    let field_types: Vec<_> = fields.iter().map(|field| field.ty.clone()).collect();
    let field_names: Vec<_> = field_idents.iter().map(ToString::to_string).collect();
    let field_labels: Vec<_> = field_names
        .iter()
        .map(|name| split_identifier(name))
        .collect();

    Ok(quote! {
        impl engine::game_module::GameResource for #name {
            const RESOURCE_ID: &'static str = #resource_id;
            const DISPLAY_NAME: &'static str = #display_name;

            fn schema() -> engine::game_module::GameResourceSchema {
                let defaults = Self::default();
                let mut default_fields = ::std::collections::BTreeMap::new();
                #(
                    default_fields.insert(
                        #field_names.to_owned(),
                        <#field_types as engine::game_module::GameField>::to_value(&defaults.#field_idents),
                    );
                )*
                engine::game_module::GameResourceSchema {
                    id: Self::RESOURCE_ID.to_owned(),
                    display_name: Self::DISPLAY_NAME.to_owned(),
                    description: format!("Project-local Rust resource `{}`.", Self::DISPLAY_NAME),
                    version: 1,
                    fields: vec![
                        #(
                            engine::game_module::FieldSchema {
                                name: #field_names.to_owned(),
                                display_name: #field_labels.to_owned(),
                                description: format!("Public setting `{}`.", #field_names),
                                field_type: <#field_types as engine::game_module::GameField>::field_type(),
                                required: true,
                                default_value: Some(
                                    <#field_types as engine::game_module::GameField>::to_value(
                                        &defaults.#field_idents,
                                    ),
                                ),
                            }
                        ),*
                    ],
                    default_value: engine::game_module::Value::Object(default_fields),
                }
            }

            fn from_value(value: &engine::game_module::Value) -> Result<Self, String> {
                let fields = match value {
                    engine::game_module::Value::Object(fields) => fields,
                    _ => return Err("resource value must be an object".to_owned()),
                };
                Ok(Self {
                    #(
                        #field_idents: <#field_types as engine::game_module::GameField>::from_value(
                            fields.get(#field_names).ok_or_else(|| {
                                format!("required field `{}` is missing", #field_names)
                            })?,
                        ).map_err(|error| format!("field `{}`: {error}", #field_names))?,
                    )*
                })
            }

            fn to_value(&self) -> engine::game_module::Value {
                let mut fields = ::std::collections::BTreeMap::new();
                #(
                    fields.insert(
                        #field_names.to_owned(),
                        <#field_types as engine::game_module::GameField>::to_value(&self.#field_idents),
                    );
                )*
                engine::game_module::Value::Object(fields)
            }
        }

        engine::inventory::submit! {
            engine::game_module::GameResourceRegistration {
                schema: <#name as engine::game_module::GameResource>::schema,
            }
        }
    })
}

fn game_component_field_is_skipped(field: &syn::Field) -> syn::Result<bool> {
    let mut skipped = false;
    for attribute in field
        .attrs
        .iter()
        .filter(|attribute| attribute.path().is_ident("game_field"))
    {
        let mut has_option = false;
        attribute.parse_nested_meta(|meta| {
            has_option = true;
            if !meta.path.is_ident("skip") {
                return Err(meta.error("supported option is `skip`"));
            }
            if skipped {
                return Err(meta.error("duplicate `skip` option"));
            }
            skipped = true;
            Ok(())
        })?;
        if !has_option {
            return Err(syn::Error::new_spanned(
                attribute,
                "game_field requires the `skip` option",
            ));
        }
    }
    Ok(skipped)
}

fn derive_game_component_impl(input: DeriveInput) -> syn::Result<proc_macro2::TokenStream> {
    let name = input.ident.clone();
    let mut display_name = None;
    let mut category = "Game".to_owned();

    for attribute in input
        .attrs
        .iter()
        .filter(|attr| attr.path().is_ident("game_component"))
    {
        attribute.parse_nested_meta(|meta| {
            if meta.path.is_ident("display_name") {
                display_name = Some(meta.value()?.parse::<syn::LitStr>()?.value());
            } else if meta.path.is_ident("category") {
                category = meta.value()?.parse::<syn::LitStr>()?.value();
            } else {
                return Err(meta.error("supported keys are display_name and category"));
            }
            Ok(())
        })?;
    }

    let resolved = resolve_component_id(&name.to_string())?;
    let type_id = resolved.component_id;
    let metadata_dependency = {
        let path = syn::LitStr::new(
            &resolved.metadata_path.to_string_lossy(),
            proc_macro2::Span::call_site(),
        );
        quote! {
            // Keep Cargo's dependency graph aware of sidecar-only edits. The
            // string is not exposed through the runtime module descriptor.
            const _: &str = include_str!(#path);
        }
    };
    let display_name = display_name.unwrap_or_else(|| split_identifier(&name.to_string()));
    let fields = match input.data {
        syn::Data::Struct(data) => match data.fields {
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

    let mut authoring_fields = Vec::new();
    for field in &fields {
        if !game_component_field_is_skipped(field)? {
            authoring_fields.push(field);
        }
    }
    let has_skipped_fields = authoring_fields.len() != fields.len();
    let field_idents: Vec<_> = authoring_fields
        .iter()
        .map(|field| field.ident.clone().expect("named fields have identifiers"))
        .collect();
    let field_types: Vec<_> = authoring_fields
        .iter()
        .map(|field| field.ty.clone())
        .collect();
    let field_names: Vec<_> = field_idents.iter().map(ToString::to_string).collect();
    let field_labels: Vec<_> = field_names
        .iter()
        .map(|name| split_identifier(name))
        .collect();
    let schema_defaults = if field_idents.is_empty() {
        quote! {
            let default_fields = ::std::collections::BTreeMap::new();
        }
    } else {
        quote! {
            let defaults = Self::default();
            let mut default_fields = ::std::collections::BTreeMap::new();
            #(
                default_fields.insert(
                    #field_names.to_owned(),
                    <#field_types as engine::game_module::GameField>::to_value(&defaults.#field_idents),
                );
            )*
        }
    };
    let from_authoring_value = if !has_skipped_fields {
        quote! {
            Ok(Self {
                #(
                    #field_idents: <#field_types as engine::game_module::GameField>::from_value(
                        fields.get(#field_names).ok_or_else(|| {
                            format!("required field `{}` is missing", #field_names)
                        })?,
                    ).map_err(|error| format!("field `{}`: {error}", #field_names))?,
                )*
            })
        }
    } else if field_idents.is_empty() {
        quote! {
            let _ = fields;
            Ok(Self::default())
        }
    } else {
        quote! {
            let mut component = Self::default();
            #(
                component.#field_idents =
                    <#field_types as engine::game_module::GameField>::from_value(
                        fields.get(#field_names).ok_or_else(|| {
                            format!("required field `{}` is missing", #field_names)
                        })?,
                    ).map_err(|error| format!("field `{}`: {error}", #field_names))?;
            )*
            Ok(component)
        }
    };
    let to_authoring_value = if field_idents.is_empty() {
        quote! {
            engine::game_module::Value::Object(::std::collections::BTreeMap::new())
        }
    } else {
        quote! {
            let mut fields = ::std::collections::BTreeMap::new();
            #(
                fields.insert(
                    #field_names.to_owned(),
                    <#field_types as engine::game_module::GameField>::to_value(&self.#field_idents),
                );
            )*
            engine::game_module::Value::Object(fields)
        }
    };

    Ok(quote! {
        #metadata_dependency

        impl engine::game_module::GameComponent for #name {
            const TYPE_ID: &'static str = #type_id;
            const DISPLAY_NAME: &'static str = #display_name;

            fn schema() -> engine::game_module::ComponentSchema {
                #schema_defaults
                engine::game_module::ComponentSchema {
                    type_id: engine::game_module::ComponentTypeId::new(Self::TYPE_ID),
                    display_name: Self::DISPLAY_NAME.to_owned(),
                    description: format!("Project-local Rust component `{}`.", Self::DISPLAY_NAME),
                    category: #category.to_owned(),
                    version: 1,
                    fields: vec![
                        #(
                            engine::game_module::FieldSchema {
                                name: #field_names.to_owned(),
                                display_name: #field_labels.to_owned(),
                                description: format!("Public setting `{}`.", #field_names),
                                field_type: <#field_types as engine::game_module::GameField>::field_type(),
                                required: true,
                                default_value: Some(
                                    <#field_types as engine::game_module::GameField>::to_value(
                                        &defaults.#field_idents,
                                    ),
                                ),
                            }
                        ),*
                    ],
                    component_default: Some(engine::game_module::Value::Object(default_fields)),
                }
            }

            fn from_authoring_value(
                value: &engine::game_module::Value,
            ) -> Result<Self, String> {
                let fields = match value {
                    engine::game_module::Value::Object(fields) => fields,
                    _ => return Err("component value must be an object".to_owned()),
                };
                #from_authoring_value
            }

            fn to_authoring_value(&self) -> engine::game_module::Value {
                #to_authoring_value
            }
        }

        engine::inventory::submit! {
            engine::game_module::GameComponentRegistration {
                schema: <#name as engine::game_module::GameComponent>::schema,
                validate: engine::game_module::validate_component_ffi::<#name>,
            }
        }
    })
}

struct ResolvedComponentId {
    component_id: String,
    metadata_path: PathBuf,
}

fn resolve_component_id(rust_type: &str) -> syn::Result<ResolvedComponentId> {
    let manifest_dir = std::env::var_os("CARGO_MANIFEST_DIR").map(PathBuf::from);
    let Some(manifest_dir) = manifest_dir else {
        return Err(syn::Error::new(
            proc_macro2::Span::call_site(),
            "GameComponent requires an adjacent `.rs.meta.json` sidecar",
        ));
    };
    let mut sidecars = Vec::new();
    let asset_source_root = manifest_dir
        .parent()
        .map(|project_root| project_root.join("assets/scripts/rust"));
    if let Some(source_root) = asset_source_root {
        collect_component_sidecars(&source_root, &mut sidecars)
            .map_err(|message| syn::Error::new(proc_macro2::Span::call_site(), message))?;
    }
    // Workspace crates and tests declare components beside their own sources.
    collect_component_sidecars(&manifest_dir.join("src"), &mut sidecars)
        .map_err(|message| syn::Error::new(proc_macro2::Span::call_site(), message))?;
    sidecars.sort();

    let mut ids = BTreeMap::<String, PathBuf>::new();
    let mut matches = Vec::new();
    for sidecar in sidecars {
        let metadata = read_component_sidecar(&sidecar)
            .map_err(|message| syn::Error::new(proc_macro2::Span::call_site(), message))?;
        if let Some(previous) = ids.insert(metadata.component_id.clone(), sidecar.clone()) {
            return Err(syn::Error::new(
                proc_macro2::Span::call_site(),
                format!(
                    "duplicate component ID `{}` in {} and {}",
                    metadata.component_id,
                    previous.display(),
                    sidecar.display()
                ),
            ));
        }
        let source_path = source_path_for_sidecar(&sidecar).ok_or_else(|| {
            syn::Error::new(
                proc_macro2::Span::call_site(),
                format!(
                    "component sidecar {} has no paired `.rs` path",
                    sidecar.display()
                ),
            )
        })?;
        let source = fs::read_to_string(&source_path).map_err(|error| {
            syn::Error::new(
                proc_macro2::Span::call_site(),
                format!(
                    "could not read component source paired with {}: {error}",
                    sidecar.display()
                ),
            )
        })?;
        if source_declares_component(&source, rust_type)? {
            matches.push((sidecar, metadata.component_id));
        }
    }

    match matches.as_slice() {
        [(metadata_path, component_id)] => Ok(ResolvedComponentId {
            component_id: component_id.clone(),
            metadata_path: metadata_path.clone(),
        }),
        [] => Err(syn::Error::new(
            proc_macro2::Span::call_site(),
            format!(
                "GameComponent `{rust_type}` has no adjacent `.rs.meta.json` sidecar; restore metadata instead of generating a replacement ID"
            ),
        )),
        _ => Err(syn::Error::new(
            proc_macro2::Span::call_site(),
            format!(
                "GameComponent type `{rust_type}` is declared by more than one sidecar-backed source"
            ),
        )),
    }
}

struct MacroComponentMetadata {
    component_id: String,
}

fn collect_component_sidecars(directory: &Path, output: &mut Vec<PathBuf>) -> Result<(), String> {
    if !directory.exists() {
        return Ok(());
    }
    let entries = fs::read_dir(directory)
        .map_err(|error| format!("could not scan {}: {error}", directory.display()))?;
    for entry in entries {
        let entry =
            entry.map_err(|error| format!("could not scan {}: {error}", directory.display()))?;
        let file_type = entry
            .file_type()
            .map_err(|error| format!("could not inspect {}: {error}", entry.path().display()))?;
        if file_type.is_symlink() {
            continue;
        }
        if file_type.is_dir() {
            collect_component_sidecars(&entry.path(), output)?;
        } else if entry
            .file_name()
            .to_string_lossy()
            .ends_with(".rs.meta.json")
        {
            output.push(entry.path());
        }
    }
    Ok(())
}

fn read_component_sidecar(path: &Path) -> Result<MacroComponentMetadata, String> {
    let json = fs::read_to_string(path)
        .map_err(|error| format!("could not read {}: {error}", path.display()))?;
    let value: serde_json::Value = serde_json::from_str(&json)
        .map_err(|error| format!("invalid component metadata {}: {error}", path.display()))?;
    let object = value.as_object().ok_or_else(|| {
        format!(
            "component metadata {} must be a JSON object",
            path.display()
        )
    })?;
    let schema_version = object
        .get("schema_version")
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| {
            format!(
                "component metadata {} has no integer `schema_version`",
                path.display()
            )
        })?;
    if schema_version != 1 {
        return Err(format!(
            "component metadata {} uses unsupported schema version {schema_version}",
            path.display()
        ));
    }
    let component_id = object
        .get("component_id")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| {
            format!(
                "component metadata {} has no string `component_id`",
                path.display()
            )
        })?
        .to_owned();
    validate_macro_component_id(&component_id)
        .map_err(|reason| format!("invalid component metadata {}: {reason}", path.display()))?;
    Ok(MacroComponentMetadata { component_id })
}

fn validate_macro_component_id(component_id: &str) -> Result<(), String> {
    if !component_id.starts_with("game.")
        || component_id.split('.').any(|segment| {
            let mut bytes = segment.bytes();
            !bytes.next().is_some_and(|byte| byte.is_ascii_lowercase())
                || !bytes
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
        })
    {
        return Err(format!(
            "component ID `{component_id}` must use a valid lowercase `game.*` namespace"
        ));
    }
    if let Some(suffix) = component_id.strip_prefix("game.c_") {
        let is_valid = suffix.len() == 26
            && suffix
                .as_bytes()
                .first()
                .is_some_and(|byte| matches!(byte, b'0'..=b'7'))
            && suffix.bytes().all(|byte| {
                matches!(
                    byte,
                    b'0'..=b'9'
                        | b'a'..=b'h'
                        | b'j'
                        | b'k'
                        | b'm'
                        | b'n'
                        | b'p'..=b't'
                        | b'v'..=b'z'
                )
            });
        if !is_valid {
            return Err(format!(
                "component ID `{component_id}` must end in a 26-character lowercase Crockford ULID"
            ));
        }
    }
    Ok(())
}

fn source_path_for_sidecar(sidecar: &Path) -> Option<PathBuf> {
    let file_name = sidecar.file_name()?.to_str()?;
    let source_name = file_name.strip_suffix(".meta.json")?;
    Some(sidecar.with_file_name(source_name))
}

fn source_declares_component(source: &str, rust_type: &str) -> syn::Result<bool> {
    let file = syn::parse_file(source)?;
    Ok(items_declare_component(&file.items, rust_type))
}

fn items_declare_component(items: &[Item], rust_type: &str) -> bool {
    items.iter().any(|item| match item {
        Item::Struct(item) => {
            item.ident == rust_type && attributes_mark_game_component(&item.attrs)
        }
        Item::Enum(item) => item.ident == rust_type && attributes_mark_game_component(&item.attrs),
        Item::Mod(item) => item
            .content
            .as_ref()
            .is_some_and(|(_, items)| items_declare_component(items, rust_type)),
        _ => false,
    })
}

fn attributes_mark_game_component(attributes: &[Attribute]) -> bool {
    attributes.iter().any(|attribute| {
        if attribute.path().is_ident("game_component") {
            return true;
        }
        if !attribute.path().is_ident("derive") {
            return false;
        }
        let mut found = false;
        let _ = attribute.parse_nested_meta(|meta| {
            if meta
                .path
                .segments
                .last()
                .is_some_and(|segment| segment.ident == "GameComponent")
            {
                found = true;
            }
            Ok(())
        });
        found
    })
}

/// Registers a query-scoped project-local Rust system.
///
/// Typed functions receive values implementing `engine::game_api::GameSystemParam`.
/// Their complete access manifest is generated from those parameter types.
#[proc_macro_attribute]
pub fn game_system(args: TokenStream, item: TokenStream) -> TokenStream {
    match game_system_impl(args.into(), parse_macro_input!(item as ItemFn)) {
        Ok(tokens) => tokens.into(),
        Err(error) => error.to_compile_error().into(),
    }
}

fn game_system_impl(
    args: proc_macro2::TokenStream,
    function: ItemFn,
) -> syn::Result<proc_macro2::TokenStream> {
    let parser = syn::punctuated::Punctuated::<Meta, Token![,]>::parse_terminated;
    let metas = parser.parse2(args)?;
    let mut schedule = None;
    let mut order: i32 = 0;
    let mut id = None;
    let mut display_name = None;
    let mut description = None;
    let mut before = Vec::new();
    let mut after = Vec::new();
    let mut aliases = Vec::new();
    for meta in metas {
        let Meta::NameValue(value) = meta else {
            return Err(syn::Error::new_spanned(meta, "expected name = value"));
        };
        if value.path.is_ident("schedule") {
            let Expr::Lit(expr) = value.value else {
                return Err(syn::Error::new_spanned(value, "schedule must be a string"));
            };
            let Lit::Str(lit) = expr.lit else {
                return Err(syn::Error::new_spanned(expr, "schedule must be a string"));
            };
            schedule = Some(lit.value());
        } else if value.path.is_ident("order") {
            let Expr::Lit(expr) = value.value else {
                return Err(syn::Error::new_spanned(value, "order must be an integer"));
            };
            let Lit::Int(lit) = expr.lit else {
                return Err(syn::Error::new_spanned(expr, "order must be an integer"));
            };
            order = lit.base10_parse()?;
        } else if value.path.is_ident("id") {
            id = Some(parse_string_expr(&value.value, "id")?);
        } else if value.path.is_ident("display_name") {
            display_name = Some(parse_string_expr(&value.value, "display_name")?);
        } else if value.path.is_ident("description") {
            description = Some(parse_string_expr(&value.value, "description")?);
        } else if value.path.is_ident("before") {
            before = parse_string_array(&value.value, "before")?;
        } else if value.path.is_ident("after") {
            after = parse_string_array(&value.value, "after")?;
        } else if value.path.is_ident("aliases") {
            aliases = parse_string_array(&value.value, "aliases")?;
        } else {
            return Err(syn::Error::new_spanned(
                value.path,
                "supported keys are id, display_name, description, schedule, order, before, after, and aliases; system access is derived from typed parameters",
            ));
        }
    }
    let schedule = match schedule.as_deref().unwrap_or("update") {
        "update" => quote!(engine::game_module::GameSystemSchedule::Update),
        "fixed_update" => quote!(engine::game_module::GameSystemSchedule::FixedUpdate),
        other => {
            return Err(syn::Error::new_spanned(
                &function.sig.ident,
                format!("unsupported schedule `{other}`"),
            ))
        }
    };
    let name = &function.sig.ident;
    let id = id.unwrap_or_else(|| format!("game.{name}"));
    let display_name = display_name.unwrap_or_else(|| split_identifier(&name.to_string()));
    let description =
        description.unwrap_or_else(|| format!("Project-local Rust system `{display_name}`."));
    let wrapper = format_ident!("__iroha_run_{}", name);
    let parameter_types = function
        .sig
        .inputs
        .iter()
        .map(|argument| match argument {
            FnArg::Typed(argument) => Ok((*argument.ty).clone()),
            FnArg::Receiver(receiver) => Err(syn::Error::new_spanned(
                receiver,
                "game systems must be free functions",
            )),
        })
        .collect::<syn::Result<Vec<_>>>()?;
    let parameter_names = (0..parameter_types.len())
        .map(|index| format_ident!("__iroha_param_{index}"))
        .collect::<Vec<_>>();
    let access_name = format_ident!("__iroha_access_{}", name);
    let invoke_name = format_ident!("__iroha_invoke_{}", name);
    let call = match &function.sig.output {
        ReturnType::Default => quote! {
            #name(#(#parameter_names),*);
            Ok(())
        },
        ReturnType::Type(_, _) => quote! {
            #name(#(#parameter_names),*)
                .map_err(|error| engine::game_api::GameApiError::System(error.to_string()))
        },
    };
    let invocation_adapter = quote! {
        #[doc(hidden)]
        fn #access_name() -> engine::game_io::GameSystemAccess {
            let mut access = engine::game_io::GameSystemAccess::default();
            #(
                <#parameter_types as engine::game_api::GameSystemParam>::declare(&mut access);
            )*
            access
        }

        #[doc(hidden)]
        fn #invoke_name(
            input: &engine::game_io::GameInvocation,
            output: engine::game_api::TypedOutput,
        ) -> Result<(), engine::game_api::GameApiError> {
            #(
                let #parameter_names =
                    <#parameter_types as engine::game_api::GameSystemParam>::fetch(
                        input,
                        output.clone(),
                    )?;
            )*
            #call
        }
    };
    let access = quote!(#access_name);
    let runner = quote!(engine::game_module::run_typed_system_ffi);
    let callback = quote!(#invoke_name);
    Ok(quote! {
        #function

        #invocation_adapter

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
                #runner(
                    input_json,
                    input_json_len,
                    output,
                    error_buffer,
                    error_buffer_len,
                    #callback,
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
    })
}

fn parse_string_expr(expression: &Expr, label: &str) -> syn::Result<String> {
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

fn parse_string_array(expression: &Expr, label: &str) -> syn::Result<Vec<String>> {
    let Expr::Array(array) = expression else {
        return Err(syn::Error::new_spanned(
            expression,
            format!("{label} must be an array of strings"),
        ));
    };
    array
        .elems
        .iter()
        .map(|value| parse_string_expr(value, label))
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
    use super::game_component_field_is_skipped;
    use quote::quote;
    use syn::parse::Parser;

    fn parse_named_field(tokens: proc_macro2::TokenStream) -> syn::Field {
        syn::Field::parse_named.parse2(tokens).unwrap()
    }

    #[test]
    fn game_component_field_is_authored_by_default() {
        let field = parse_named_field(quote!(pub speed: f64));

        assert!(!game_component_field_is_skipped(&field).unwrap());
    }

    #[test]
    fn game_component_field_skip_is_detected() {
        let field = parse_named_field(quote!(
            #[game_field(skip)]
            runtime_cache: RuntimeCache
        ));

        assert!(game_component_field_is_skipped(&field).unwrap());
    }

    #[test]
    fn game_component_field_rejects_unknown_options() {
        let field = parse_named_field(quote!(
            #[game_field(read_only)]
            pub speed: f64
        ));

        let error = game_component_field_is_skipped(&field).unwrap_err();
        assert!(error.to_string().contains("supported option is `skip`"));
    }

    #[test]
    fn game_component_field_rejects_empty_attributes() {
        let field = parse_named_field(quote!(
            #[game_field()]
            pub speed: f64
        ));

        let error = game_component_field_is_skipped(&field).unwrap_err();
        assert!(error
            .to_string()
            .contains("game_field requires the `skip` option"));
    }
}
