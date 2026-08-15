use quote::{format_ident, quote, ToTokens};
use std::collections::{BTreeMap, BTreeSet};
use syn::{GenericArgument, Ident, PathArguments, Type, TypePath};

#[derive(Clone)]
struct ComponentRequirement {
    ty: Type,
    required: bool,
    write: bool,
}

#[derive(Clone)]
enum QueryValue {
    Entity,
    Component {
        // Boxed so `Component` does not dominate the size of every `QueryValue`.
        ty: Box<Type>,
        optional: bool,
        write: bool,
        field: Ident,
    },
    Transform {
        optional: bool,
        write: bool,
        field: Ident,
    },
}

pub(super) struct AutoQuery {
    spec: Ident,
    row: Ident,
    wrapper: Ident,
    query_id: String,
    tuple: bool,
    values: Vec<QueryValue>,
    components: BTreeMap<String, ComponentRequirement>,
    without: Vec<Type>,
    transform_required: bool,
    transform_optional: bool,
    transform_write: bool,
    component_write: bool,
}

impl AutoQuery {
    pub(super) fn wrapper_type(&self) -> syn::Result<Type> {
        let wrapper = &self.wrapper;
        syn::parse2(quote!(&mut #wrapper))
    }

    pub(super) fn definition(&self) -> proc_macro2::TokenStream {
        let spec = &self.spec;
        let row = &self.row;
        let wrapper = &self.wrapper;
        let query_id = &self.query_id;
        let component_access = self.components.values().map(|requirement| {
            let ty = &requirement.ty;
            match (requirement.required, requirement.write) {
                (true, true) => quote!(builder = builder.write::<#ty>();),
                (true, false) => quote!(builder = builder.read::<#ty>();),
                (false, true) => quote!(builder = builder.optional_write::<#ty>();),
                (false, false) => quote!(builder = builder.optional::<#ty>();),
            }
        });
        let transform_access = if self.transform_required {
            quote!(builder = builder.view::<engine::game_api::LocalTransformView>();)
        } else if self.transform_optional {
            quote!(builder = builder.optional_view::<engine::game_api::LocalTransformView>();)
        } else {
            quote!()
        };
        let row_fields = self.values.iter().filter_map(|value| match value {
            QueryValue::Entity => None,
            QueryValue::Component {
                ty,
                optional,
                field,
                ..
            } => {
                let ty = if *optional {
                    quote!(Option<#ty>)
                } else {
                    quote!(#ty)
                };
                Some(quote!(#field: #ty,))
            }
            QueryValue::Transform {
                optional, field, ..
            } => {
                let ty = if *optional {
                    quote!(Option<engine::game_each::Transform>)
                } else {
                    quote!(engine::game_each::Transform)
                };
                Some(quote!(#field: #ty,))
            }
        });
        let query_field = self
            .component_write
            .then(|| quote!(__iroha_query: engine::game_api::Query<#spec>,));
        let commands_field = self
            .transform_write
            .then(|| quote!(__iroha_commands: engine::game_api::Commands,));
        let without_checks = self.without.iter().map(|ty| {
            quote! {
                if __iroha_raw_row.optional_component::<#ty>()?.is_some() {
                    continue;
                }
            }
        });
        let decodes = self.values.iter().filter_map(|value| match value {
            QueryValue::Entity => None,
            QueryValue::Component {
                ty,
                optional,
                field,
                ..
            } => Some(if *optional {
                quote!(let #field = __iroha_raw_row.optional_component::<#ty>()?;)
            } else {
                quote!(let #field = __iroha_raw_row.component::<#ty>()?;)
            }),
            QueryValue::Transform {
                optional, field, ..
            } => Some(if *optional {
                quote! {
                    let #field = __iroha_raw_row
                        .optional_view::<engine::game_api::LocalTransformView>()?
                        .map(engine::game_each::Transform::from);
                }
            } else {
                quote! {
                    let #field = engine::game_each::Transform::from(
                        __iroha_raw_row.view::<engine::game_api::LocalTransformView>()?,
                    );
                }
            }),
        });
        let row_initializers = self.values.iter().filter_map(|value| match value {
            QueryValue::Entity => None,
            QueryValue::Component { field, .. } | QueryValue::Transform { field, .. } => {
                Some(quote!(#field,))
            }
        });
        let new_commands_parameter = self
            .transform_write
            .then(|| quote!(, __iroha_commands: engine::game_api::Commands));
        let new_query_field = self
            .component_write
            .then(|| quote!(__iroha_query: __iroha_source_query,));
        let new_commands_field = self.transform_write.then(|| quote!(__iroha_commands,));
        let mutable = self.has_writable_values();
        let iterator_receiver = if mutable {
            quote!(&mut self)
        } else {
            quote!(&self)
        };
        let iterator = if mutable {
            quote!(self.__iroha_rows.iter_mut())
        } else {
            quote!(self.__iroha_rows.iter())
        };
        let item_types = self.values.iter().map(QueryValue::item_type);
        let item_values = self.values.iter().map(QueryValue::borrow);
        let item_type = if self.tuple {
            quote!((#(#item_types,)*))
        } else {
            quote!(#(#item_types)*)
        };
        let item_value = if self.tuple {
            quote!((#(#item_values,)*))
        } else {
            quote!(#(#item_values)*)
        };
        let flush = self.flush();

        quote! {
            #[doc(hidden)]
            #[allow(non_camel_case_types)]
            struct #spec;

            impl engine::game_api::QuerySpec for #spec {
                const ID: &'static str = #query_id;

                fn access() -> engine::game_io::GameQueryAccess {
                    let mut builder = engine::game_api::QueryAccessBuilder::new(Self::ID);
                    #(#component_access)*
                    #transform_access
                    builder.build()
                }
            }

            #[doc(hidden)]
            #[allow(non_camel_case_types)]
            struct #row {
                __iroha_entity: engine::game_io::GameEntityHandle,
                #(#row_fields)*
            }

            #[doc(hidden)]
            #[allow(non_camel_case_types)]
            pub struct #wrapper {
                #query_field
                __iroha_rows: Vec<#row>,
                #commands_field
            }

            impl #wrapper {
                fn __iroha_new(
                    __iroha_source_query: engine::game_api::Query<#spec>
                    #new_commands_parameter
                ) -> Result<Self, engine::game_api::GameApiError> {
                    let mut __iroha_rows = Vec::new();
                    for __iroha_raw_row in __iroha_source_query.rows() {
                        #(#without_checks)*
                        #(#decodes)*
                        __iroha_rows.push(#row {
                            __iroha_entity: __iroha_raw_row.entity(),
                            #(#row_initializers)*
                        });
                    }
                    Ok(Self {
                        #new_query_field
                        __iroha_rows,
                        #new_commands_field
                    })
                }

                pub fn iter(
                    #iterator_receiver
                ) -> impl Iterator<Item = #item_type> + '_ {
                    #iterator.map(|__iroha_row| #item_value)
                }

                #flush
            }
        }
    }

    pub(super) fn declare(&self) -> proc_macro2::TokenStream {
        let spec = &self.spec;
        let transform_command = self.transform_write.then(|| {
            quote! {
                if !access
                    .command_families
                    .contains(&engine::game_io::GameCommandFamily::Transform)
                {
                    access
                        .command_families
                        .push(engine::game_io::GameCommandFamily::Transform);
                }
            }
        });
        quote! {
            access.queries.push(<#spec as engine::game_api::QuerySpec>::access());
            #transform_command
        }
    }

    pub(super) fn fetch(
        &self,
        local: &Ident,
        input: &Ident,
        output: &Ident,
    ) -> proc_macro2::TokenStream {
        let spec = &self.spec;
        let wrapper = &self.wrapper;
        let commands = self.transform_write.then(|| {
            quote! {
                let __iroha_commands =
                    <engine::game_api::Commands as engine::game_api::GameSystemParam>::fetch(
                        #input,
                        #output.clone(),
                    )?;
            }
        });
        let commands_argument = self.transform_write.then(|| quote!(, __iroha_commands));
        quote! {
            let __iroha_query =
                <engine::game_api::Query<#spec> as engine::game_api::GameSystemParam>::fetch(
                    #input,
                    #output.clone(),
                )?;
            #commands
            let mut #local = #wrapper::__iroha_new(__iroha_query #commands_argument)?;
        }
    }

    pub(super) fn argument(&self, local: &Ident) -> proc_macro2::TokenStream {
        quote!(&mut #local)
    }

    pub(super) fn flush_call(&self, local: &Ident) -> proc_macro2::TokenStream {
        quote!(#local.__iroha_flush()?;)
    }

    fn has_writable_values(&self) -> bool {
        self.component_write || self.transform_write
    }

    fn flush(&self) -> proc_macro2::TokenStream {
        if !self.has_writable_values() {
            return quote! {
                fn __iroha_flush(self) -> Result<(), engine::game_api::GameApiError> {
                    let _ = self;
                    Ok(())
                }
            };
        }

        let query_pattern = self.component_write.then(|| quote!(mut __iroha_query,));
        let commands_pattern = self.transform_write.then(|| quote!(mut __iroha_commands,));
        let patches = self.values.iter().filter_map(QueryValue::patch);
        quote! {
            fn __iroha_flush(self) -> Result<(), engine::game_api::GameApiError> {
                let Self {
                    #query_pattern
                    __iroha_rows,
                    #commands_pattern
                } = self;
                for __iroha_row in __iroha_rows {
                    let __iroha_entity = __iroha_row.__iroha_entity;
                    #(#patches)*
                }
                Ok(())
            }
        }
    }
}

impl QueryValue {
    fn item_type(&self) -> proc_macro2::TokenStream {
        match self {
            Self::Entity => quote!(engine::game_each::Entity),
            Self::Component {
                ty,
                optional: false,
                write: false,
                ..
            } => quote!(&#ty),
            Self::Component {
                ty,
                optional: false,
                write: true,
                ..
            } => quote!(&mut #ty),
            Self::Component {
                ty,
                optional: true,
                write: false,
                ..
            } => quote!(Option<&#ty>),
            Self::Component {
                ty,
                optional: true,
                write: true,
                ..
            } => quote!(Option<&mut #ty>),
            Self::Transform {
                optional: false,
                write: false,
                ..
            } => quote!(&engine::game_each::Transform),
            Self::Transform {
                optional: false,
                write: true,
                ..
            } => quote!(&mut engine::game_each::Transform),
            Self::Transform {
                optional: true,
                write: false,
                ..
            } => quote!(Option<&engine::game_each::Transform>),
            Self::Transform {
                optional: true,
                write: true,
                ..
            } => quote!(Option<&mut engine::game_each::Transform>),
        }
    }

    fn borrow(&self) -> proc_macro2::TokenStream {
        match self {
            Self::Entity => quote!(__iroha_row.__iroha_entity),
            Self::Component {
                optional: false,
                write: false,
                field,
                ..
            }
            | Self::Transform {
                optional: false,
                write: false,
                field,
            } => quote!(&__iroha_row.#field),
            Self::Component {
                optional: false,
                write: true,
                field,
                ..
            }
            | Self::Transform {
                optional: false,
                write: true,
                field,
            } => quote!(&mut __iroha_row.#field),
            Self::Component {
                optional: true,
                write: false,
                field,
                ..
            }
            | Self::Transform {
                optional: true,
                write: false,
                field,
            } => quote!(__iroha_row.#field.as_ref()),
            Self::Component {
                optional: true,
                write: true,
                field,
                ..
            }
            | Self::Transform {
                optional: true,
                write: true,
                field,
            } => quote!(__iroha_row.#field.as_mut()),
        }
    }

    fn patch(&self) -> Option<proc_macro2::TokenStream> {
        match self {
            Self::Component {
                optional: false,
                write: true,
                field,
                ..
            } => Some(quote! {
                __iroha_query.set(__iroha_entity, __iroha_row.#field)?;
            }),
            Self::Component {
                optional: true,
                write: true,
                field,
                ..
            } => Some(quote! {
                if let Some(__iroha_value) = __iroha_row.#field {
                    __iroha_query.set(__iroha_entity, __iroha_value)?;
                }
            }),
            Self::Transform {
                optional: false,
                write: true,
                field,
            } => Some(quote! {
                __iroha_commands.set_transform(
                    __iroha_entity,
                    __iroha_row.#field.translation,
                    __iroha_row.#field.rotation,
                    __iroha_row.#field.scale,
                );
            }),
            Self::Transform {
                optional: true,
                write: true,
                field,
            } => Some(quote! {
                if let Some(__iroha_value) = __iroha_row.#field {
                    __iroha_commands.set_transform(
                        __iroha_entity,
                        __iroha_value.translation,
                        __iroha_value.rotation,
                        __iroha_value.scale,
                    );
                }
            }),
            _ => None,
        }
    }
}

pub(super) fn parse(
    ty: &Type,
    system_name: &Ident,
    query_index: usize,
    system_id: &str,
) -> syn::Result<Option<AutoQuery>> {
    let Some(arguments) = query_arguments(ty)? else {
        return Ok(None);
    };
    let data = &arguments[0];
    if arguments.len() == 1 && !is_automatic_data(data) {
        return Ok(None);
    }
    if arguments.len() > 2 {
        return Err(syn::Error::new_spanned(
            ty,
            "automatic Query accepts data and an optional filter type",
        ));
    }

    let tuple = matches!(unparenthesized(data), Type::Tuple(_));
    let data_types = match unparenthesized(data) {
        Type::Tuple(tuple) => {
            if tuple.elems.is_empty() {
                return Err(syn::Error::new_spanned(
                    tuple,
                    "automatic Query data tuple must not be empty",
                ));
            }
            tuple.elems.iter().cloned().collect::<Vec<_>>()
        }
        single => vec![single.clone()],
    };

    let mut values = Vec::new();
    let mut components = BTreeMap::<String, ComponentRequirement>::new();
    let mut data_components = BTreeSet::new();
    let mut transform_seen = false;
    let mut transform_required = false;
    let mut transform_optional = false;
    let mut transform_write = false;
    let mut component_write = false;
    for (index, data_type) in data_types.iter().enumerate() {
        let value = classify_data(data_type, index)?;
        match &value {
            QueryValue::Entity => {}
            QueryValue::Component {
                ty,
                optional,
                write,
                ..
            } => {
                let key = type_key(ty);
                if !data_components.insert(key) {
                    return Err(syn::Error::new_spanned(
                        data_type,
                        "automatic Query data cannot contain the same component twice",
                    ));
                }
                add_requirement(&mut components, ty, !optional, *write);
                component_write |= *write;
            }
            QueryValue::Transform {
                optional, write, ..
            } => {
                if std::mem::replace(&mut transform_seen, true) {
                    return Err(syn::Error::new_spanned(
                        data_type,
                        "automatic Query data cannot contain Transform twice",
                    ));
                }
                transform_required |= !optional;
                transform_optional |= *optional;
                transform_write |= *write;
            }
        }
        values.push(value);
    }

    let mut without = Vec::new();
    let mut positive_filters = BTreeSet::new();
    let mut negative_filters = BTreeSet::new();
    if let Some(filter) = arguments.get(1) {
        for filter in filter_types(filter)? {
            match classify_filter(&filter)? {
                QueryFilter::With(ty) => {
                    let key = type_key(&ty);
                    if negative_filters.contains(&key) {
                        return Err(syn::Error::new_spanned(
                            filter,
                            "the same component cannot be used by both With and Without",
                        ));
                    }
                    positive_filters.insert(key);
                    add_requirement(&mut components, &ty, true, false);
                }
                QueryFilter::Without(ty) => {
                    let key = type_key(&ty);
                    if data_components.contains(&key) || positive_filters.contains(&key) {
                        return Err(syn::Error::new_spanned(
                            filter,
                            "Without cannot exclude a component required by Query data or With",
                        ));
                    }
                    if negative_filters.insert(key) {
                        add_requirement(&mut components, &ty, false, false);
                        without.push(ty);
                    }
                }
            }
        }
    }

    let suffix = format!("{system_name}_{query_index}");
    Ok(Some(AutoQuery {
        spec: format_ident!("__IrohaQuerySpec_{suffix}"),
        row: format_ident!("__IrohaQueryRow_{suffix}"),
        wrapper: format_ident!("__IrohaQuery_{suffix}"),
        query_id: format!("{system_id}.query.{query_index}"),
        tuple,
        values,
        components,
        without,
        transform_required,
        transform_optional,
        transform_write,
        component_write,
    }))
}

fn query_arguments(ty: &Type) -> syn::Result<Option<Vec<Type>>> {
    let Type::Path(path) = unparenthesized(ty) else {
        return Ok(None);
    };
    let Some(segment) = path.path.segments.last() else {
        return Ok(None);
    };
    if segment.ident != "Query" {
        return Ok(None);
    }
    let PathArguments::AngleBracketed(arguments) = &segment.arguments else {
        return Err(syn::Error::new_spanned(
            segment,
            "Query requires at least one type argument",
        ));
    };
    let mut types = Vec::new();
    for argument in &arguments.args {
        match argument {
            GenericArgument::Type(ty) => types.push(ty.clone()),
            other => {
                return Err(syn::Error::new_spanned(
                    other,
                    "Query accepts type arguments only",
                ))
            }
        }
    }
    if types.is_empty() {
        return Err(syn::Error::new_spanned(
            arguments,
            "Query requires at least one type argument",
        ));
    }
    Ok(Some(types))
}

fn is_automatic_data(ty: &Type) -> bool {
    let ty = unparenthesized(ty);
    matches!(ty, Type::Reference(_) | Type::Tuple(_))
        || named(ty, "Entity")
        || named(ty, "GameEntityHandle")
        || named(ty, "Option")
}

fn classify_data(ty: &Type, index: usize) -> syn::Result<QueryValue> {
    let ty = unparenthesized(ty);
    if named(ty, "Entity") || named(ty, "GameEntityHandle") {
        return Ok(QueryValue::Entity);
    }
    if let Type::Reference(reference) = ty {
        return referenced_data(
            &reference.elem,
            false,
            reference.mutability.is_some(),
            index,
        );
    }
    if let Some(inner) = single_argument(ty, "Option")? {
        let Type::Reference(reference) = unparenthesized(&inner) else {
            return Err(syn::Error::new_spanned(
                inner,
                "automatic Query supports Option<&T> or Option<&mut T>",
            ));
        };
        return referenced_data(&reference.elem, true, reference.mutability.is_some(), index);
    }
    Err(syn::Error::new_spanned(
        ty,
        "automatic Query data supports Entity, &T, &mut T, Option<&T>, and Option<&mut T>",
    ))
}

fn referenced_data(
    ty: &Type,
    optional: bool,
    write: bool,
    index: usize,
) -> syn::Result<QueryValue> {
    let ty = unparenthesized(ty).clone();
    let field = format_ident!("__iroha_field_{index}");
    if named(&ty, "Transform") {
        Ok(QueryValue::Transform {
            optional,
            write,
            field,
        })
    } else {
        Ok(QueryValue::Component {
            ty: Box::new(ty),
            optional,
            write,
            field,
        })
    }
}

enum QueryFilter {
    With(Type),
    Without(Type),
}

fn filter_types(ty: &Type) -> syn::Result<Vec<Type>> {
    match unparenthesized(ty) {
        Type::Tuple(tuple) if tuple.elems.is_empty() => Ok(Vec::new()),
        Type::Tuple(tuple) => Ok(tuple.elems.iter().cloned().collect()),
        single => Ok(vec![single.clone()]),
    }
}

fn classify_filter(ty: &Type) -> syn::Result<QueryFilter> {
    if let Some(inner) = single_argument(ty, "With")? {
        reject_transform_filter(&inner)?;
        return Ok(QueryFilter::With(inner));
    }
    if let Some(inner) = single_argument(ty, "Without")? {
        reject_transform_filter(&inner)?;
        return Ok(QueryFilter::Without(inner));
    }
    Err(syn::Error::new_spanned(
        ty,
        "automatic Query filters support With<T>, Without<T>, or a tuple of them",
    ))
}

fn reject_transform_filter(ty: &Type) -> syn::Result<()> {
    if named(ty, "Transform") {
        Err(syn::Error::new_spanned(
            ty,
            "With and Without accept project components, not Transform",
        ))
    } else {
        Ok(())
    }
}

fn add_requirement(
    components: &mut BTreeMap<String, ComponentRequirement>,
    ty: &Type,
    required: bool,
    write: bool,
) {
    let key = type_key(ty);
    components
        .entry(key)
        .and_modify(|current| {
            current.required |= required;
            current.write |= write;
        })
        .or_insert_with(|| ComponentRequirement {
            ty: ty.clone(),
            required,
            write,
        });
}

fn single_argument(ty: &Type, wrapper: &str) -> syn::Result<Option<Type>> {
    let Type::Path(path) = unparenthesized(ty) else {
        return Ok(None);
    };
    let Some(segment) = path.path.segments.last() else {
        return Ok(None);
    };
    if segment.ident != wrapper {
        return Ok(None);
    }
    let PathArguments::AngleBracketed(arguments) = &segment.arguments else {
        return Err(syn::Error::new_spanned(
            segment,
            format!("`{wrapper}` requires one type argument"),
        ));
    };
    let types = arguments
        .args
        .iter()
        .filter_map(|argument| match argument {
            GenericArgument::Type(ty) => Some(ty.clone()),
            _ => None,
        })
        .collect::<Vec<_>>();
    match types.as_slice() {
        [inner] => Ok(Some(inner.clone())),
        _ => Err(syn::Error::new_spanned(
            arguments,
            format!("`{wrapper}` requires exactly one type argument"),
        )),
    }
}

fn type_key(ty: &Type) -> String {
    ty.to_token_stream().to_string()
}

fn named(ty: &Type, name: &str) -> bool {
    let Type::Path(TypePath { qself: None, path }) = unparenthesized(ty) else {
        return false;
    };
    path.segments
        .last()
        .is_some_and(|segment| segment.ident == name)
}

fn unparenthesized(mut ty: &Type) -> &Type {
    loop {
        match ty {
            Type::Group(group) => ty = &group.elem,
            Type::Paren(paren) => ty = &paren.elem,
            _ => return ty,
        }
    }
}
