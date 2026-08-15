mod param;

use crate::{register, SystemConfig};
use param::{add_requirement, classify, ComponentRequirement, EachParam, EachParamKind};
use quote::{format_ident, quote};
use std::collections::BTreeMap;
use syn::{FnArg, ItemFn, ReturnType, Type};

pub(crate) fn expand(
    function: ItemFn,
    config: SystemConfig,
) -> syn::Result<proc_macro2::TokenStream> {
    let name = &function.sig.ident;
    let params = function
        .sig
        .inputs
        .iter()
        .enumerate()
        .map(|(index, argument)| match argument {
            FnArg::Typed(argument) => Ok(EachParam {
                local: format_ident!("__iroha_each_param_{index}"),
                kind: classify(&argument.ty)?,
            }),
            FnArg::Receiver(receiver) => Err(syn::Error::new_spanned(
                receiver,
                "game systems must be free functions",
            )),
        })
        .collect::<syn::Result<Vec<_>>>()?;
    if params
        .iter()
        .all(|param| matches!(param.kind, EachParamKind::Global(_)))
    {
        return Err(syn::Error::new_spanned(
            &function.sig,
            "`game_system(each)` requires at least one entity parameter",
        ));
    }

    let mut components = BTreeMap::<String, ComponentRequirement>::new();
    let mut transform_required = false;
    let mut transform_optional = false;
    let mut transform_write = false;
    let mut globals = Vec::<Type>::new();
    for param in &params {
        match &param.kind {
            EachParamKind::Entity => {}
            EachParamKind::Component {
                ty,
                optional,
                write,
            } => add_requirement(&mut components, ty, !optional, *write),
            EachParamKind::Transform { optional, write } => {
                transform_required |= !optional;
                transform_optional |= *optional;
                transform_write |= *write;
            }
            EachParamKind::With(ty) => add_requirement(&mut components, ty, true, false),
            EachParamKind::Without(ty) => add_requirement(&mut components, ty, false, false),
            EachParamKind::AnyOf { members, .. } => {
                for ty in members {
                    add_requirement(&mut components, ty, false, false);
                }
            }
            EachParamKind::Global(ty) => globals.push(ty.clone()),
        }
    }

    let query_write = components.values().any(|requirement| requirement.write);
    let query_mutability = query_write.then(|| quote!(mut));
    let query = format_ident!("__IrohaEachQuery_{}", name);
    let query_id = format!("{}.each", config.id);
    let component_access = components.values().map(|requirement| {
        let ty = &requirement.ty;
        match (requirement.required, requirement.write) {
            (true, true) => quote!(builder = builder.write::<#ty>();),
            (true, false) => quote!(builder = builder.read::<#ty>();),
            (false, true) => quote!(builder = builder.optional_write::<#ty>();),
            (false, false) => quote!(builder = builder.optional::<#ty>();),
        }
    });
    let transform_access = if transform_required {
        quote!(builder = builder.view::<engine::game_api::LocalTransformView>();)
    } else if transform_optional {
        quote!(builder = builder.optional_view::<engine::game_api::LocalTransformView>();)
    } else {
        quote!()
    };
    let query_spec = quote! {
        #[doc(hidden)]
        #[allow(non_camel_case_types)]
        struct #query;

        impl engine::game_api::QuerySpec for #query {
            const ID: &'static str = #query_id;

            fn access() -> engine::game_io::GameQueryAccess {
                let mut builder = engine::game_api::QueryAccessBuilder::new(Self::ID);
                #(#component_access)*
                #transform_access
                builder.build()
            }
        }
    };

    let access = format_ident!("__iroha_access_{}", name);
    let invoke = format_ident!("__iroha_invoke_{}", name);
    let command_access = transform_write.then(|| {
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
    let transform_commands = transform_write.then(|| {
        quote! {
            let mut __iroha_transform_commands =
                <engine::game_api::Commands as engine::game_api::GameSystemParam>::fetch(
                    input,
                    output.clone(),
                )?;
        }
    });

    let entity_params = params
        .iter()
        .filter(|param| !matches!(param.kind, EachParamKind::Global(_)))
        .collect::<Vec<_>>();
    let locals = entity_params.iter().map(|param| &param.local);
    let patterns = entity_params.iter().map(|param| {
        let local = &param.local;
        if param.kind.is_mutable() {
            quote!(mut #local)
        } else {
            quote!(#local)
        }
    });
    let decodes = entity_params
        .iter()
        .map(|param| param.decode())
        .collect::<syn::Result<Vec<_>>>()?;
    let global_fetches = params.iter().filter_map(|param| {
        let EachParamKind::Global(ty) = &param.kind else {
            return None;
        };
        let local = &param.local;
        Some(quote! {
            let #local = <#ty as engine::game_api::GameSystemParam>::fetch(
                input,
                output.clone(),
            )?;
        })
    });
    let arguments = params.iter().map(|param| param.argument());
    let call = match &function.sig.output {
        ReturnType::Default => quote!(#name(#(#arguments),*);),
        ReturnType::Type(_, _) => quote! {
            #name(#(#arguments),*)
                .map_err(|error| engine::game_api::GameApiError::System(error.to_string()))?;
        },
    };
    let patches = params.iter().filter_map(|param| param.patch());

    let adapter = quote! {
        #query_spec

        #[doc(hidden)]
        fn #access() -> engine::game_io::GameSystemAccess {
            let mut access = engine::game_io::GameSystemAccess::default();
            #(<#globals as engine::game_api::GameSystemParam>::declare(&mut access);)*
            access.queries.push(<#query as engine::game_api::QuerySpec>::access());
            #command_access
            access
        }

        #[doc(hidden)]
        fn #invoke(
            input: &engine::game_io::GameInvocation,
            output: engine::game_api::TypedOutput,
        ) -> Result<(), engine::game_api::GameApiError> {
            let #query_mutability __iroha_query =
                <engine::game_api::Query<#query> as engine::game_api::GameSystemParam>::fetch(
                    input,
                    output.clone(),
                )?;
            #transform_commands
            let __iroha_row_count = __iroha_query.rows().len();
            for __iroha_row_index in 0..__iroha_row_count {
                let (__iroha_entity, #(#patterns),*) = {
                    let __iroha_row = &__iroha_query.rows()[__iroha_row_index];
                    #(#decodes)*
                    (__iroha_row.entity(), #(#locals),*)
                };
                #(#global_fetches)*
                #call
                #(#patches)*
            }
            Ok(())
        }
    };

    Ok(register(function, adapter, access, invoke, config))
}
