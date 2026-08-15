use quote::{quote, ToTokens};
use std::collections::BTreeMap;
use syn::{GenericArgument, PathArguments, Type, TypePath};

#[derive(Clone)]
pub(super) struct ComponentRequirement {
    pub(super) ty: Type,
    pub(super) required: bool,
    pub(super) write: bool,
}

#[derive(Clone)]
pub(super) enum EachParamKind {
    Entity,
    Component {
        ty: Type,
        optional: bool,
        write: bool,
    },
    Transform {
        optional: bool,
        write: bool,
    },
    With(Type),
    Without(Type),
    AnyOf {
        marker: Type,
        members: Vec<Type>,
    },
    Global(Type),
}

pub(super) struct EachParam {
    pub(super) local: syn::Ident,
    pub(super) kind: EachParamKind,
}

impl EachParamKind {
    pub(super) fn is_mutable(&self) -> bool {
        matches!(
            self,
            Self::Component { write: true, .. } | Self::Transform { write: true, .. }
        )
    }
}

impl EachParam {
    pub(super) fn decode(&self) -> syn::Result<proc_macro2::TokenStream> {
        let local = &self.local;
        Ok(match &self.kind {
            EachParamKind::Entity => quote!(let #local = __iroha_row.entity();),
            EachParamKind::Component {
                ty,
                optional: false,
                ..
            } => quote!(let #local = __iroha_row.component::<#ty>()?;),
            EachParamKind::Component {
                ty, optional: true, ..
            } => quote!(let #local = __iroha_row.optional_component::<#ty>()?;),
            EachParamKind::Transform {
                optional: false, ..
            } => quote! {
                let #local = engine::game_each::Transform::from(
                    __iroha_row.view::<engine::game_api::LocalTransformView>()?,
                );
            },
            EachParamKind::Transform { optional: true, .. } => quote! {
                let #local = __iroha_row
                    .optional_view::<engine::game_api::LocalTransformView>()?
                    .map(engine::game_each::Transform::from);
            },
            EachParamKind::With(ty) => {
                quote!(let #local = engine::game_each::With::<#ty>::new();)
            }
            EachParamKind::Without(ty) => quote! {
                if __iroha_row.optional_component::<#ty>()?.is_some() {
                    continue;
                }
                let #local = engine::game_each::Without::<#ty>::new();
            },
            EachParamKind::AnyOf { marker, members } => {
                let reads = members.iter().map(|ty| {
                    quote! {
                        if let Some(__iroha_value) = __iroha_row.optional_component::<#ty>()? {
                            #local.insert(__iroha_value);
                        }
                    }
                });
                quote! {
                    let mut #local: engine::game_each::AnyOf<#marker> =
                        engine::game_each::AnyOf::new();
                    #(#reads)*
                    if #local.is_empty() {
                        continue;
                    }
                }
            }
            EachParamKind::Global(_) => {
                return Err(syn::Error::new_spanned(
                    local,
                    "internal macro error: global parameter decoded as entity data",
                ));
            }
        })
    }

    pub(super) fn argument(&self) -> proc_macro2::TokenStream {
        let local = &self.local;
        match &self.kind {
            EachParamKind::Component {
                optional: false,
                write: false,
                ..
            }
            | EachParamKind::Transform {
                optional: false,
                write: false,
            } => quote!(&#local),
            EachParamKind::Component {
                optional: false,
                write: true,
                ..
            }
            | EachParamKind::Transform {
                optional: false,
                write: true,
            } => quote!(&mut #local),
            EachParamKind::Component {
                optional: true,
                write: false,
                ..
            }
            | EachParamKind::Transform {
                optional: true,
                write: false,
            } => quote!(#local.as_ref()),
            EachParamKind::Component {
                optional: true,
                write: true,
                ..
            }
            | EachParamKind::Transform {
                optional: true,
                write: true,
            } => quote!(#local.as_mut()),
            EachParamKind::Entity
            | EachParamKind::With(_)
            | EachParamKind::Without(_)
            | EachParamKind::AnyOf { .. }
            | EachParamKind::Global(_) => quote!(#local),
        }
    }

    pub(super) fn patch(&self) -> Option<proc_macro2::TokenStream> {
        let local = &self.local;
        match &self.kind {
            EachParamKind::Component {
                optional: false,
                write: true,
                ..
            } => Some(quote!(__iroha_query.set(__iroha_entity, #local)?;)),
            EachParamKind::Component {
                optional: true,
                write: true,
                ..
            } => Some(quote! {
                if let Some(__iroha_value) = #local {
                    __iroha_query.set(__iroha_entity, __iroha_value)?;
                }
            }),
            EachParamKind::Transform {
                optional: false,
                write: true,
            } => Some(transform_patch(local, false)),
            EachParamKind::Transform {
                optional: true,
                write: true,
            } => Some(transform_patch(local, true)),
            _ => None,
        }
    }
}

fn transform_patch(local: &syn::Ident, optional: bool) -> proc_macro2::TokenStream {
    if optional {
        quote! {
            if let Some(__iroha_value) = #local {
                __iroha_transform_commands.set_transform(
                    __iroha_entity,
                    __iroha_value.translation,
                    __iroha_value.rotation,
                    __iroha_value.scale,
                );
            }
        }
    } else {
        quote! {
            __iroha_transform_commands.set_transform(
                __iroha_entity,
                #local.translation,
                #local.rotation,
                #local.scale,
            );
        }
    }
}

pub(super) fn classify(ty: &Type) -> syn::Result<EachParamKind> {
    let ty = unparenthesized(ty);
    if let Type::Reference(reference) = ty {
        return referenced(&reference.elem, false, reference.mutability.is_some());
    }
    if named(ty, "Entity") || named(ty, "GameEntityHandle") {
        return Ok(EachParamKind::Entity);
    }
    if let Some(inner) = single_argument(ty, "Option")? {
        let Type::Reference(reference) = unparenthesized(&inner) else {
            return Err(syn::Error::new_spanned(
                inner,
                "`each` supports `Option<&T>` or `Option<&mut T>`",
            ));
        };
        return referenced(&reference.elem, true, reference.mutability.is_some());
    }
    if let Some(inner) = single_argument(ty, "With")? {
        return Ok(EachParamKind::With(inner));
    }
    if let Some(inner) = single_argument(ty, "Without")? {
        return Ok(EachParamKind::Without(inner));
    }
    if let Some(marker) = single_argument(ty, "AnyOf")? {
        let Type::Tuple(tuple) = unparenthesized(&marker) else {
            return Err(syn::Error::new_spanned(
                marker,
                "`AnyOf` requires a tuple such as `AnyOf<(&Player, &Enemy)>`",
            ));
        };
        if tuple.elems.is_empty() {
            return Err(syn::Error::new_spanned(
                tuple,
                "`AnyOf` requires at least one component reference",
            ));
        }
        let members = tuple
            .elems
            .iter()
            .map(|member| {
                let Type::Reference(reference) = unparenthesized(member) else {
                    return Err(syn::Error::new_spanned(
                        member,
                        "`AnyOf` members must be shared component references",
                    ));
                };
                if reference.mutability.is_some() {
                    return Err(syn::Error::new_spanned(
                        member,
                        "mutable `AnyOf` members are not supported; use a separate `&mut T`",
                    ));
                }
                let target = unparenthesized(&reference.elem).clone();
                if named(&target, "Transform") {
                    return Err(syn::Error::new_spanned(
                        target,
                        "`AnyOf` accepts project components, not `Transform`",
                    ));
                }
                Ok(target)
            })
            .collect::<syn::Result<Vec<_>>>()?;
        return Ok(EachParamKind::AnyOf { marker, members });
    }
    Ok(EachParamKind::Global(ty.clone()))
}

fn referenced(ty: &Type, optional: bool, write: bool) -> syn::Result<EachParamKind> {
    let ty = unparenthesized(ty).clone();
    if named(&ty, "Transform") {
        Ok(EachParamKind::Transform { optional, write })
    } else {
        Ok(EachParamKind::Component {
            ty,
            optional,
            write,
        })
    }
}

pub(super) fn add_requirement(
    components: &mut BTreeMap<String, ComponentRequirement>,
    ty: &Type,
    required: bool,
    write: bool,
) {
    let key = ty.to_token_stream().to_string();
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
