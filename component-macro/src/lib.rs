use proc_macro::TokenStream;
use quote::{format_ident, quote};
use syn::punctuated::Punctuated;
use syn::token::Comma;
use syn::{DeriveInput, Path, parse_macro_input};

#[proc_macro_derive(SdComponent, attributes(component))]
pub fn derive_component(input: TokenStream) -> TokenStream {
    let DeriveInput { ident, attrs, .. } = parse_macro_input!(input);

    let component_attr = attrs
        .iter()
        .find(|a| a.path().is_ident("component"))
        .expect("missing #[component(...)]");

    let trait_paths: Punctuated<Path, Comma> = component_attr
        .parse_args_with(Punctuated::parse_terminated)
        .expect("invalid attribute syntax");

    let methods = trait_paths.iter().map(|path| {
        let trait_ident = &path.segments.last().unwrap().ident;
        let method_name = format_ident!("as_{}", trait_ident.to_string().to_lowercase());
        quote! {
            fn #method_name(self: std::sync::Arc<Self>) -> Result<std::sync::Arc<dyn #path>, sdk::component::ComponentError> {
                Ok(self)
            }
        }
    });

    quote! {
        impl sdk::component::SdComponent for #ident {
            #(#methods)*
        }
    }
    .into()
}
