use proc_macro::TokenStream;

#[proc_macro_attribute]
#[cfg(feature = "otlp-tracing")]
pub fn trace(attr: TokenStream, item: TokenStream) -> TokenStream {
    use quote::quote;
    use syn::{parse_macro_input, ItemFn};

    let additional_fields = if !attr.is_empty() {
        let attr_tokens: proc_macro2::TokenStream = attr.into();
        quote! {, #attr_tokens }
    } else {
        quote! {}
    };

    let function = parse_macro_input!(item as ItemFn);

    let fields_tokens = quote! {
        fields(module = module_path!(), file = file!(), line = line!() #additional_fields)
    };

    let expanded = quote! {
        #[tracing::instrument(skip_all, #fields_tokens)]
        #function
    };

    expanded.into()
}

#[proc_macro_attribute]
#[cfg(not(feature = "otlp-tracing"))]
pub fn trace(_attr: TokenStream, item: TokenStream) -> TokenStream {
    item
}
