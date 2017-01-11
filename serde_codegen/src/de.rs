use syn::{self, aster};
use quote::Tokens;

use bound;
use internals::ast::{Body, Field, Item, Style, Variant};
use internals::{self, attr};

use std::iter;

pub fn expand_derive_deserialize(item: &syn::MacroInput) -> Result<Tokens, String> {
    let item = {
        let ctxt = internals::Ctxt::new();
        let item = Item::from_ast(&ctxt, item);
        check_no_str(&ctxt, &item);
        try!(ctxt.check());
        item
    };

    let impl_generics = build_impl_generics(&item);

    let ty = aster::ty().path()
        .segment(item.ident.clone()).with_generics(impl_generics.clone()).build()
        .build();

    let body = deserialize_body(&item,
                                &impl_generics,
                                ty.clone());

    let where_clause = &impl_generics.where_clause;

    let dummy_const = aster::id(format!("_IMPL_DESERIALIZE_FOR_{}", item.ident));

    Ok(quote! {
        #[allow(non_upper_case_globals, unused_attributes, unused_qualifications)]
        const #dummy_const: () = {
            extern crate serde as _serde;
            #[automatically_derived]
            impl #impl_generics _serde::Deserialize for #ty #where_clause {
                fn deserialize<__D>(deserializer: &mut __D) -> ::std::result::Result<#ty, __D::Error>
                    where __D: _serde::Deserializer
                #body
            }
        };
    })
}

// All the generics in the input, plus a bound `T: Deserialize` for each generic
// field type that will be deserialized by us, plus a bound `T: Default` for
// each generic field type that will be set to a default value.
fn build_impl_generics(item: &Item) -> syn::Generics {
    let generics = bound::without_defaults(item.generics);

    let generics = bound::with_where_predicates_from_fields(
        item, &generics,
        |attrs| attrs.de_bound());

    match item.attrs.de_bound() {
        Some(predicates) => {
            bound::with_where_predicates(&generics, predicates)
        }
        None => {
            let generics = bound::with_bound(item, &generics,
                needs_deserialize_bound,
                &aster::path().ids(&["_serde", "de", "Deserialize"]).build());
            bound::with_bound(item, &generics,
                requires_default,
                &aster::path().global().ids(&["std", "default", "Default"]).build())
        }
    }
}

// Fields with a `skip_deserializing` or `deserialize_with` attribute are not
// deserialized by us so we do not generate a bound. Fields with a `bound`
// attribute specify their own bound so we do not generate one. All other fields
// may need a `T: Deserialize` bound where T is the type of the field.
fn needs_deserialize_bound(attrs: &attr::Field) -> bool {
    !attrs.skip_deserializing()
        && attrs.deserialize_with().is_none()
        && attrs.de_bound().is_none()
}

// Fields with a `default` attribute (not `default=...`), and fields with a
// `skip_deserializing` attribute that do not also have `default=...`.
fn requires_default(attrs: &attr::Field) -> bool {
    attrs.default() == &attr::FieldDefault::Default
}

fn deserialize_body(
    item: &Item,
    impl_generics: &syn::Generics,
    ty: syn::Ty,
) -> Tokens {
    match item.body {
        Body::Enum(ref variants) => {
            deserialize_item_enum(
                &item.ident,
                impl_generics,
                ty,
                variants,
                &item.attrs)
        }
        Body::Struct(Style::Struct, ref fields) => {
            if fields.iter().any(|field| field.ident.is_none()) {
                panic!("struct has unnamed fields");
            }

            deserialize_struct(
                &item.ident,
                None,
                impl_generics,
                ty,
                fields,
                &item.attrs)
        }
        Body::Struct(Style::Tuple, ref fields) |
        Body::Struct(Style::Newtype, ref fields) => {
            if fields.iter().any(|field| field.ident.is_some()) {
                panic!("tuple struct has named fields");
            }

            deserialize_tuple(
                &item.ident,
                None,
                impl_generics,
                ty,
                fields,
                &item.attrs)
        }
        Body::Struct(Style::Unit, _) => {
            deserialize_unit_struct(
                &item.ident,
                &item.attrs)
        }
    }
}

// Build `__Visitor<A, B, ...>(PhantomData<A>, PhantomData<B>, ...)`
//
// Returns:
//
//     1. the struct declaration
//     2. the visitor type, including generics
//     3. the expression for instantiating the visitor
fn deserialize_visitor(generics: &syn::Generics) -> (Tokens, Tokens, Tokens) {
    if generics.lifetimes.is_empty() && generics.ty_params.is_empty() {
        (
            quote! {
                struct __Visitor;
            },
            quote!(__Visitor),
            quote!(__Visitor),
        )
    } else {
        let where_clause = &generics.where_clause;

        let num_phantoms = generics.lifetimes.len() + generics.ty_params.len();

        let phantom_types = generics.lifetimes.iter()
            .map(|lifetime_def| {
                let lifetime = &lifetime_def.lifetime;
                quote!(::std::marker::PhantomData<& #lifetime ()>)
            }).chain(generics.ty_params.iter()
                .map(|ty_param| {
                    let ident = &ty_param.ident;
                    quote!(::std::marker::PhantomData<#ident>)
                }));

        let all_params = generics.lifetimes.iter()
            .map(|lifetime_def| {
                let lifetime = &lifetime_def.lifetime;
                quote!(#lifetime)
            }).chain(generics.ty_params.iter()
                .map(|ty_param| {
                    let ident = &ty_param.ident;
                    quote!(#ident)
                }));

        let ty_param_idents: Vec<_> = generics.ty_params.iter()
            .map(|t| {
                let ident = &t.ident;
                quote!(#ident)
            })
            .collect();

        let ty_param_idents = if ty_param_idents.is_empty() {
            None
        } else {
            Some(quote!(::<#(#ty_param_idents),*>))
        };

        let phantom_exprs = iter::repeat(quote!(::std::marker::PhantomData)).take(num_phantoms);

        (
            quote! {
                struct __Visitor #generics ( #(#phantom_types),* ) #where_clause;
            },
            quote!(__Visitor <#(#all_params),*> ),
            quote!(__Visitor #ty_param_idents ( #(#phantom_exprs),* )),
        )
    }
}

fn deserialize_unit_struct(
    type_ident: &syn::Ident,
    item_attrs: &attr::Item,
) -> Tokens {
    let type_name = item_attrs.name().deserialize_name();

    quote!({
        struct __Visitor;

        impl _serde::de::Visitor for __Visitor {
            type Value = #type_ident;

            #[inline]
            fn visit_unit<__E>(&mut self) -> ::std::result::Result<#type_ident, __E>
                where __E: _serde::de::Error,
            {
                Ok(#type_ident)
            }

            #[inline]
            fn visit_seq<__V>(&mut self, mut visitor: __V) -> ::std::result::Result<#type_ident, __V::Error>
                where __V: _serde::de::SeqVisitor,
            {
                try!(visitor.end());
                self.visit_unit()
            }
        }

        deserializer.deserialize_unit_struct(#type_name, __Visitor)
    })
}

fn deserialize_tuple(
    type_ident: &syn::Ident,
    variant_ident: Option<&syn::Ident>,
    impl_generics: &syn::Generics,
    ty: syn::Ty,
    fields: &[Field],
    item_attrs: &attr::Item,
) -> Tokens {
    let where_clause = &impl_generics.where_clause;

    let (visitor_item, visitor_ty, visitor_expr) = deserialize_visitor(impl_generics);

    let is_enum = variant_ident.is_some();
    let type_path = match variant_ident {
        Some(variant_ident) => quote!(#type_ident::#variant_ident),
        None => quote!(#type_ident),
    };

    let nfields = fields.len();

    let visit_newtype_struct = if !is_enum && nfields == 1 {
        Some(deserialize_newtype_struct(
            type_ident,
            &type_path,
            impl_generics,
            &fields[0],
        ))
    } else {
        None
    };

    let visit_seq = deserialize_seq(
        type_ident,
        &type_path,
        impl_generics,
        fields,
        false,
    );

    let dispatch = if is_enum {
        quote!(visitor.visit_tuple(#nfields, #visitor_expr))
    } else if nfields == 1 {
        let type_name = item_attrs.name().deserialize_name();
        quote!(deserializer.deserialize_newtype_struct(#type_name, #visitor_expr))
    } else {
        let type_name = item_attrs.name().deserialize_name();
        quote!(deserializer.deserialize_tuple_struct(#type_name, #nfields, #visitor_expr))
    };

    quote!({
        #visitor_item

        impl #impl_generics _serde::de::Visitor for #visitor_ty #where_clause {
            type Value = #ty;

            #visit_newtype_struct

            #[inline]
            fn visit_seq<__V>(&mut self, mut visitor: __V) -> ::std::result::Result<#ty, __V::Error>
                where __V: _serde::de::SeqVisitor
            {
                #visit_seq
            }
        }

        #dispatch
    })
}

fn deserialize_seq(
    type_ident: &syn::Ident,
    type_path: &Tokens,
    impl_generics: &syn::Generics,
    fields: &[Field],
    is_struct: bool,
) -> Tokens {
    let mut index_in_seq = 0usize;
    let let_values: Vec<_> = fields.iter()
        .enumerate()
        .map(|(i, field)| {
            let name = aster::id(format!("__field{}", i));
            if field.attrs.skip_deserializing() {
                let default = expr_is_missing(&field.attrs);
                quote! {
                    let #name = #default;
                }
            } else {
                let visit = match field.attrs.deserialize_with() {
                    None => {
                        let field_ty = &field.ty;
                        quote!(try!(visitor.visit::<#field_ty>()))
                    }
                    Some(path) => {
                        let (wrapper, wrapper_impl, wrapper_ty) = wrap_deserialize_with(
                            type_ident, impl_generics, field.ty, path);
                        quote!({
                            #wrapper
                            #wrapper_impl
                            try!(visitor.visit::<#wrapper_ty>()).map(|wrap| wrap.value)
                        })
                    }
                };
                let assign = quote! {
                    let #name = match #visit {
                        Some(value) => { value },
                        None => {
                            try!(visitor.end());
                            return Err(_serde::de::Error::invalid_length(#index_in_seq));
                        }
                    };
                };
                index_in_seq += 1;
                assign
            }
        })
        .collect();

    let result = if is_struct {
        let args = fields.iter()
            .enumerate()
            .map(|(i, field)| {
                let ident = field.ident.clone().expect("struct contains unnamed fields");
                let value = aster::id(format!("__field{}", i));
                quote!(#ident: #value)
            });
        quote! {
            #type_path { #(#args),* }
        }
    } else {
        let args = (0..fields.len()).map(|i| aster::id(format!("__field{}", i)));
        quote! {
            #type_path ( #(#args),* )
        }
    };

    quote! {
        #(#let_values)*

        try!(visitor.end());

        Ok(#result)
    }
}

fn deserialize_newtype_struct(
    type_ident: &syn::Ident,
    type_path: &Tokens,
    impl_generics: &syn::Generics,
    field: &Field,
) -> Tokens {
    let value = match field.attrs.deserialize_with() {
        None => {
            let field_ty = &field.ty;
            quote! {
                try!(<#field_ty as _serde::Deserialize>::deserialize(__e))
            }
        }
        Some(path) => {
            let (wrapper, wrapper_impl, wrapper_ty) = wrap_deserialize_with(
                type_ident, impl_generics, field.ty, path);
            quote!({
                #wrapper
                #wrapper_impl
                try!(<#wrapper_ty as _serde::Deserialize>::deserialize(__e)).value
            })
        }
    };
    quote! {
        #[inline]
        fn visit_newtype_struct<__E>(&mut self, __e: &mut __E) -> ::std::result::Result<Self::Value, __E::Error>
            where __E: _serde::Deserializer,
        {
            Ok(#type_path(#value))
        }
    }
}

fn deserialize_struct(
    type_ident: &syn::Ident,
    variant_ident: Option<&syn::Ident>,
    impl_generics: &syn::Generics,
    ty: syn::Ty,
    fields: &[Field],
    item_attrs: &attr::Item,
) -> Tokens {
    let where_clause = &impl_generics.where_clause;

    let (visitor_item, visitor_ty, visitor_expr) = deserialize_visitor(impl_generics);

    let type_path = match variant_ident {
        Some(variant_ident) => quote!(#type_ident::#variant_ident),
        None => quote!(#type_ident),
    };

    let visit_seq = deserialize_seq(
        type_ident,
        &type_path,
        impl_generics,
        fields,
        true,
    );

    let (field_visitor, fields_stmt, visit_map) = deserialize_struct_visitor(
        type_ident,
        type_path,
        impl_generics,
        fields,
        item_attrs,
    );

    let is_enum = variant_ident.is_some();
    let dispatch = if is_enum {
        quote! {
            visitor.visit_struct(FIELDS, #visitor_expr)
        }
    } else {
        let type_name = item_attrs.name().deserialize_name();
        quote! {
            deserializer.deserialize_struct(#type_name, FIELDS, #visitor_expr)
        }
    };

    quote!({
        #field_visitor

        #visitor_item

        impl #impl_generics _serde::de::Visitor for #visitor_ty #where_clause {
            type Value = #ty;

            #[inline]
            fn visit_seq<__V>(&mut self, mut visitor: __V) -> ::std::result::Result<#ty, __V::Error>
                where __V: _serde::de::SeqVisitor
            {
                #visit_seq
            }

            #[inline]
            fn visit_map<__V>(&mut self, mut visitor: __V) -> ::std::result::Result<#ty, __V::Error>
                where __V: _serde::de::MapVisitor
            {
                #visit_map
            }
        }

        #fields_stmt

        #dispatch
    })
}

fn deserialize_item_enum(
    type_ident: &syn::Ident,
    impl_generics: &syn::Generics,
    ty: syn::Ty,
    variants: &[Variant],
    item_attrs: &attr::Item
) -> Tokens {
    let where_clause = &impl_generics.where_clause;

    let type_name = item_attrs.name().deserialize_name();

    let variant_visitor = deserialize_field_visitor(
        variants.iter()
            .filter(|variant| !variant.attrs.skip_deserializing())
            .map(|variant| variant.attrs.name().deserialize_name())
            .collect(),
        item_attrs,
        true,
    );

    let variant_names = variants.iter().map(|variant| variant.ident.to_string());

    let variants_stmt = quote! {
        const VARIANTS: &'static [&'static str] = &[ #(#variant_names),* ];
    };

    // Match arms to extract a variant from a string
    let mut variant_arms = vec![];
    for (i, variant) in variants.iter().filter(|variant| !variant.attrs.skip_deserializing()).enumerate() {
        let variant_name = aster::id(format!("__field{}", i));
        let variant_name = quote!(__Field::#variant_name);

        let block = deserialize_variant(
            type_ident,
            impl_generics,
            ty.clone(),
            variant,
            item_attrs,
        );

        let arm = quote! {
            #variant_name => #block
        };
        variant_arms.push(arm);
    }

    let match_variant = if variant_arms.is_empty() {
        // This is an empty enum like `enum Impossible {}` or an enum in which
        // all variants have `#[serde(skip_deserializing)]`.
        quote! {
            // FIXME: Once we drop support for Rust 1.15:
            // let Err(err) = visitor.visit_variant::<__Field>();
            // Err(err)
            visitor.visit_variant::<__Field>().map(|impossible| match impossible {})
        }
    } else {
        quote! {
            match try!(visitor.visit_variant()) {
                #(#variant_arms)*
            }
        }
    };

    let (visitor_item, visitor_ty, visitor_expr) = deserialize_visitor(impl_generics);

    quote!({
        #variant_visitor

        #visitor_item

        impl #impl_generics _serde::de::Visitor for #visitor_ty #where_clause {
            type Value = #ty;

            fn visit_enum<__V>(&mut self, mut visitor: __V) -> ::std::result::Result<#ty, __V::Error>
                where __V: _serde::de::VariantVisitor,
            {
                #match_variant
            }
        }

        #variants_stmt

        deserializer.deserialize_enum(#type_name, VARIANTS, #visitor_expr)
    })
}

fn deserialize_variant(
    type_ident: &syn::Ident,
    generics: &syn::Generics,
    ty: syn::Ty,
    variant: &Variant,
    item_attrs: &attr::Item,
) -> Tokens {
    let variant_ident = &variant.ident;

    match variant.style {
        Style::Unit => {
            quote!({
                try!(visitor.visit_unit());
                Ok(#type_ident::#variant_ident)
            })
        }
        Style::Newtype => {
            deserialize_newtype_variant(
                type_ident,
                variant_ident,
                generics,
                &variant.fields[0],
            )
        }
        Style::Tuple => {
            deserialize_tuple(
                type_ident,
                Some(variant_ident),
                generics,
                ty,
                &variant.fields,
                item_attrs,
            )
        }
        Style::Struct => {
            deserialize_struct(
                type_ident,
                Some(variant_ident),
                generics,
                ty,
                &variant.fields,
                item_attrs,
            )
        }
    }
}

fn deserialize_newtype_variant(
    type_ident: &syn::Ident,
    variant_ident: &syn::Ident,
    impl_generics: &syn::Generics,
    field: &Field,
) -> Tokens {
    let visit = match field.attrs.deserialize_with() {
        None => {
            let field_ty = &field.ty;
            quote!(try!(visitor.visit_newtype::<#field_ty>()))
        }
        Some(path) => {
            let (wrapper, wrapper_impl, wrapper_ty) = wrap_deserialize_with(
                type_ident, impl_generics, field.ty, path);
            quote!({
                #wrapper
                #wrapper_impl
                try!(visitor.visit_newtype::<#wrapper_ty>()).value
            })
        }
    };
    quote! {
        Ok(#type_ident::#variant_ident(#visit)),
    }
}

fn deserialize_field_visitor(
    field_names: Vec<String>,
    item_attrs: &attr::Item,
    is_variant: bool,
) -> Tokens {
    // Create the field names for the fields.
    let field_idents: &Vec<_> = &(0 .. field_names.len())
        .map(|i| aster::id(format!("__field{}", i)))
        .collect();

    let ignore_variant = if is_variant || item_attrs.deny_unknown_fields() {
        None
    } else {
        Some(quote!(__ignore,))
    };

    let fallthrough_arm = if is_variant {
        quote! {
            Err(_serde::de::Error::unknown_variant(value))
        }
    } else if item_attrs.deny_unknown_fields() {
        quote! {
            Err(_serde::de::Error::unknown_field(value))
        }
    } else {
        quote! {
            Ok(__Field::__ignore)
        }
    };

    quote! {
        #[allow(non_camel_case_types)]
        enum __Field {
            #(#field_idents,)*
            #ignore_variant
        }

        impl _serde::Deserialize for __Field {
            #[inline]
            fn deserialize<__D>(deserializer: &mut __D) -> ::std::result::Result<__Field, __D::Error>
                where __D: _serde::Deserializer,
            {
                struct __FieldVisitor;

                impl _serde::de::Visitor for __FieldVisitor {
                    type Value = __Field;

                    fn visit_str<__E>(&mut self, value: &str) -> ::std::result::Result<__Field, __E>
                        where __E: _serde::de::Error
                    {
                        match value {
                            #(
                                #field_names => Ok(__Field::#field_idents),
                            )*
                            _ => #fallthrough_arm
                        }
                    }
                }

                deserializer.deserialize_struct_field(__FieldVisitor)
            }
        }
    }
}

fn deserialize_struct_visitor(
    type_ident: &syn::Ident,
    struct_path: Tokens,
    impl_generics: &syn::Generics,
    fields: &[Field],
    item_attrs: &attr::Item,
) -> (Tokens, Tokens, Tokens) {
    let field_exprs = fields.iter()
        .map(|field| field.attrs.name().deserialize_name())
        .collect();

    let field_visitor = deserialize_field_visitor(
        field_exprs,
        item_attrs,
        false,
    );

    let visit_map = deserialize_map(
        type_ident,
        struct_path,
        impl_generics,
        fields,
        item_attrs,
    );

    let field_names = fields.iter().map(|field| {
        field.ident.clone().expect("struct contains unnamed field").to_string()
    });

    let fields_stmt = quote! {
        const FIELDS: &'static [&'static str] = &[ #(#field_names),* ];
    };

    (field_visitor, fields_stmt, visit_map)
}

fn deserialize_map(
    type_ident: &syn::Ident,
    struct_path: Tokens,
    impl_generics: &syn::Generics,
    fields: &[Field],
    item_attrs: &attr::Item,
) -> Tokens {
    if fields.is_empty() && item_attrs.deny_unknown_fields() {
        return quote! {
            // FIXME: Once we drop support for Rust 1.15:
            // let None::<__Field> = try!(visitor.visit_key());
            try!(visitor.visit_key::<__Field>()).map(|impossible| match impossible {});
            try!(visitor.end());
            Ok(#struct_path {})
        };
    }

    // Create the field names for the fields.
    let fields_names = fields.iter()
        .enumerate()
        .map(|(i, field)|
             (field, aster::id(format!("__field{}", i))))
        .collect::<Vec<_>>();

    // Declare each field that will be deserialized.
    let let_values: Vec<_> = fields_names.iter()
        .filter(|&&(field, _)| !field.attrs.skip_deserializing())
        .map(|&(field, ref name)| {
            let field_ty = &field.ty;
            quote! {
                let mut #name: Option<#field_ty> = None;
            }
        })
        .collect();

    // Match arms to extract a value for a field.
    let value_arms = fields_names.iter()
        .filter(|&&(field, _)| !field.attrs.skip_deserializing())
        .map(|&(field, ref name)| {
            let deser_name = field.attrs.name().deserialize_name();

            let visit = match field.attrs.deserialize_with() {
                None => {
                    let field_ty = &field.ty;
                    quote! {
                        try!(visitor.visit_value::<#field_ty>())
                    }
                }
                Some(path) => {
                    let (wrapper, wrapper_impl, wrapper_ty) = wrap_deserialize_with(
                        type_ident, impl_generics, field.ty, path);
                    quote!({
                        #wrapper
                        #wrapper_impl
                        try!(visitor.visit_value::<#wrapper_ty>()).value
                    })
                }
            };
            quote! {
                __Field::#name => {
                    if #name.is_some() {
                        return Err(<__V::Error as _serde::de::Error>::duplicate_field(#deser_name));
                    }
                    #name = Some(#visit);
                }
            }
        })
        .collect::<Vec<_>>();

    // Match arms to ignore value for fields that have `skip_deserializing`.
    // Ignored even if `deny_unknown_fields` is set.
    let skipped_arms = fields_names.iter()
        .filter(|&&(field, _)| field.attrs.skip_deserializing())
        .map(|&(_, ref name)| {
            quote! {
                __Field::#name => {
                    let _ = try!(visitor.visit_value::<_serde::de::impls::IgnoredAny>());
                }
            }
        })
        .collect::<Vec<_>>();

    // Visit ignored values to consume them
    let ignored_arm = if item_attrs.deny_unknown_fields() {
        None
    } else {
        Some(quote! {
            _ => { let _ = try!(visitor.visit_value::<_serde::de::impls::IgnoredAny>()); }
        })
    };

    let extract_values = fields_names.iter()
        .filter(|&&(field, _)| !field.attrs.skip_deserializing())
        .map(|&(field, ref name)| {
            let missing_expr = expr_is_missing(&field.attrs);

            quote! {
                let #name = match #name {
                    Some(#name) => #name,
                    None => #missing_expr
                };
            }
        })
        .collect::<Vec<_>>();

    let result = fields_names.iter()
        .map(|&(field, ref name)| {
            let ident = field.ident.clone().expect("struct contains unnamed fields");
            let value = if field.attrs.skip_deserializing() {
                expr_is_missing(&field.attrs)
            } else {
                quote!(#name)
            };
            quote!(#ident: #value)
        });

    quote! {
        #(#let_values)*

        while let Some(key) = try!(visitor.visit_key::<__Field>()) {
            match key {
                #(#value_arms)*
                #(#skipped_arms)*
                #ignored_arm
            }
        }

        try!(visitor.end());

        #(#extract_values)*

        Ok(#struct_path { #(#result),* })
    }
}

/// This function wraps the expression in `#[serde(deserialize_with="...")]` in
/// a trait to prevent it from accessing the internal `Deserialize` state.
fn wrap_deserialize_with(
    type_ident: &syn::Ident,
    impl_generics: &syn::Generics,
    field_ty: &syn::Ty,
    deserialize_with: &syn::Path,
) -> (Tokens, Tokens, syn::Path) {
    // Quasi-quoting doesn't do a great job of expanding generics into paths,
    // so manually build it.
    let wrapper_ty = aster::path()
        .segment("__SerdeDeserializeWithStruct")
            .with_generics(impl_generics.clone())
            .build()
        .build();

    let where_clause = &impl_generics.where_clause;

    let phantom_ty = aster::path()
        .segment(type_ident)
            .with_generics(aster::from_generics(impl_generics.clone())
                .strip_ty_params()
                .build())
            .build()
        .build();

    (
        quote! {
            struct __SerdeDeserializeWithStruct #impl_generics #where_clause {
                value: #field_ty,
                phantom: ::std::marker::PhantomData<#phantom_ty>,
            }
        },
        quote! {
            impl #impl_generics _serde::Deserialize for #wrapper_ty #where_clause {
                fn deserialize<__D>(__d: &mut __D) -> ::std::result::Result<Self, __D::Error>
                    where __D: _serde::Deserializer
                {
                    let value = try!(#deserialize_with(__d));
                    Ok(__SerdeDeserializeWithStruct {
                        value: value,
                        phantom: ::std::marker::PhantomData,
                    })
                }
            }
        },
        wrapper_ty,
    )
}

fn expr_is_missing(attrs: &attr::Field) -> Tokens {
    match *attrs.default() {
        attr::FieldDefault::Default => {
            return quote!(::std::default::Default::default());
        }
        attr::FieldDefault::Path(ref path) => {
            return quote!(#path());
        }
        attr::FieldDefault::None => { /* below */ }
    }

    let name = attrs.name().deserialize_name();
    match attrs.deserialize_with() {
        None => {
            quote! {
                try!(visitor.missing_field(#name))
            }
        }
        Some(_) => {
            quote! {
                return Err(<__V::Error as _serde::de::Error>::missing_field(#name))
            }
        }
    }
}

fn check_no_str(cx: &internals::Ctxt, item: &Item) {
    let fail = || {
        cx.error(
            "Serde does not support deserializing fields of type &str; \
             consider using String instead");
    };

    for field in item.body.all_fields() {
        if field.attrs.skip_deserializing()
            || field.attrs.deserialize_with().is_some() { continue }

        if let syn::Ty::Rptr(_, ref inner) = *field.ty {
            if let syn::Ty::Path(_, ref path) = inner.ty {
                if path.segments.len() == 1 && path.segments[0].ident == "str" {
                    fail();
                    return;
                }
            }
        }
    }
}
