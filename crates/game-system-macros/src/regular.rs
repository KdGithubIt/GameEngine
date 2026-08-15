mod query;

use crate::{register, SystemConfig};
use query::AutoQuery;
use quote::{format_ident, quote};
use syn::{FnArg, Ident, ItemFn, ReturnType, Type};

enum RegularParam {
    Existing { ty: Type, local: Ident },
    AutomaticQuery { query: AutoQuery, local: Ident },
}

impl RegularParam {
    fn declare(&self) -> proc_macro2::TokenStream {
        match self {
            Self::Existing { ty, .. } => {
                quote!(<#ty as engine::game_api::GameSystemParam>::declare(&mut access);)
            }
            Self::AutomaticQuery { query, .. } => query.declare(),
        }
    }

    fn fetch(&self, input: &Ident, output: &Ident) -> proc_macro2::TokenStream {
        match self {
            Self::Existing { ty, local } => quote! {
                let #local = <#ty as engine::game_api::GameSystemParam>::fetch(
                    #input,
                    #output.clone(),
                )?;
            },
            Self::AutomaticQuery { query, local } => query.fetch(local, input, output),
        }
    }

    fn argument(&self) -> proc_macro2::TokenStream {
        match self {
            Self::Existing { local, .. } => quote!(#local),
            Self::AutomaticQuery { query, local } => query.argument(local),
        }
    }

    fn flush(&self) -> Option<proc_macro2::TokenStream> {
        match self {
            Self::Existing { .. } => None,
            Self::AutomaticQuery { query, local } => Some(query.flush_call(local)),
        }
    }

    fn definition(&self) -> Option<proc_macro2::TokenStream> {
        match self {
            Self::Existing { .. } => None,
            Self::AutomaticQuery { query, .. } => Some(query.definition()),
        }
    }
}

pub(crate) fn expand(
    mut function: ItemFn,
    config: SystemConfig,
) -> syn::Result<proc_macro2::TokenStream> {
    let name = function.sig.ident.clone();
    let mut query_index = 0;
    let mut params = Vec::new();
    for (index, argument) in function.sig.inputs.iter_mut().enumerate() {
        let FnArg::Typed(argument) = argument else {
            return Err(syn::Error::new_spanned(
                argument,
                "game systems must be free functions",
            ));
        };
        let local = format_ident!("__iroha_param_{index}");
        if let Some(query) = query::parse(&argument.ty, &name, query_index, &config.id)? {
            *argument.ty = query.wrapper_type()?;
            params.push(RegularParam::AutomaticQuery { query, local });
            query_index += 1;
        } else {
            params.push(RegularParam::Existing {
                ty: (*argument.ty).clone(),
                local,
            });
        }
    }

    let definitions = params.iter().filter_map(RegularParam::definition);
    let declarations = params.iter().map(RegularParam::declare);
    let input = format_ident!("input");
    let output = format_ident!("output");
    let fetches = params.iter().map(|param| param.fetch(&input, &output));
    let arguments = params.iter().map(RegularParam::argument);
    let flushes = params.iter().filter_map(RegularParam::flush);
    let access = format_ident!("__iroha_access_{}", name);
    let invoke = format_ident!("__iroha_invoke_{}", name);
    let call = match &function.sig.output {
        ReturnType::Default => quote! {
            #name(#(#arguments),*);
        },
        ReturnType::Type(_, _) => quote! {
            #name(#(#arguments),*)
                .map_err(|error| engine::game_api::GameApiError::System(error.to_string()))?;
        },
    };
    let adapter = quote! {
        #(#definitions)*

        #[doc(hidden)]
        fn #access() -> engine::game_io::GameSystemAccess {
            let mut access = engine::game_io::GameSystemAccess::default();
            #(#declarations)*
            access
        }

        #[doc(hidden)]
        fn #invoke(
            #input: &engine::game_io::GameInvocation,
            #output: engine::game_api::TypedOutput,
        ) -> Result<(), engine::game_api::GameApiError> {
            #(#fetches)*
            #call
            #(#flushes)*
            Ok(())
        }
    };
    Ok(register(function, adapter, access, invoke, config))
}
