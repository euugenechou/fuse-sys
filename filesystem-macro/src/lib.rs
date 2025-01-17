use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::quote;
use std::collections::HashSet;
use syn::{
    parse::Parser,
    parse_macro_input,
    punctuated::Punctuated,
    token::{Comma, Semi},
    BareFnArg, Expr, Fields, GenericArgument, Ident, ItemStruct, PathArguments, ReturnType, Stmt,
    Type, TypeBareFn, TypePtr,
};

const IDENT_CHARS: &str = "_qwertyuiopasdfghjklzxcvbnmQWERTYUIOPASDFGHJKLZXCVBNM";
const PRIMITIVE_IDENTS: &[&str] = &[
    "u8", "u16", "u32", "u64", "u128", "usize", "i8", "i16", "i32", "i64", "i128", "isize",
];

fn gen_ident(base: &str) -> Ident {
    syn::parse(
        format!("{base}{}", random_string::generate(10, IDENT_CHARS))
            .parse::<TokenStream>()
            .unwrap(),
    )
    .unwrap()
}

fn is_ident(ty: &Type, ident: &str) -> bool {
    matches!(ty, Type::Path(path) if path.path.segments.last().unwrap().ident == ident)
}

struct UnsafeFnConvert {
    new_inputs: Punctuated<BareFnArg, Comma>,
    unconverted_call: Punctuated<Expr, Comma>,
    converted_call: Punctuated<Expr, Comma>,
    conversion: Punctuated<Stmt, Semi>,
    reexport_types: HashSet<String>,
}

impl UnsafeFnConvert {
    fn sub_type(ty: Type) -> Type {
        syn::parse(
            match ty {
                Type::Ptr(TypePtr {
                    mutability, elem, ..
                }) => {
                    let sub = Self::sub_type(*elem);
                    quote!(& #mutability #sub)
                }
                ty if is_ident(&ty, "c_char") => quote!(u8),
                ty => quote!(#ty),
            }
            .into(),
        )
        .unwrap()
    }

    fn new(inputs: Punctuated<BareFnArg, Comma>) -> Self {
        let mut reexport_types = HashSet::new();
        let mut new_inputs = Punctuated::new();
        let mut unconverted_call = Punctuated::new();
        let mut converted_call = Punctuated::new();
        let mut conversions: Vec<Stmt> = vec![];

        let mut lookahead = inputs.clone().into_iter().skip(1);
        let mut inputs = inputs.into_iter();

        while let Some(arg) = inputs.next() {
            let next = lookahead.next();
            let sized = matches!(&next, Some(next) if is_ident(&next.ty, "usize"));
            let size_ident = next.map(|n| n.name.unwrap().0);

            let ident = arg.name.unwrap().0;
            let new_ident = gen_ident(&ident.to_string());

            unconverted_call.push(syn::parse(quote!(#ident).into()).unwrap());
            converted_call.push(syn::parse(quote!(#new_ident).into()).unwrap());

            let new_ty: Type = match arg.ty {
                Type::Ptr(TypePtr {
                    mutability,
                    const_token,
                    elem,
                    ..
                }) if sized => {
                    let sub_ty = Self::sub_type(*elem);
                    let ty = syn::parse(quote!(&#mutability [#sub_ty]).into()).unwrap();

                    inputs.next();
                    let size_ident = size_ident.unwrap();
                    unconverted_call.push(syn::parse(quote!(#size_ident).into()).unwrap());

                    let slice_from: Ident = syn::parse(
                        if mutability.is_none() {
                            quote!(from_raw_parts)
                        } else {
                            quote!(from_raw_parts_mut)
                        }
                        .into(),
                    )
                    .unwrap();

                    conversions.push(
                        syn::parse(quote!(let #new_ident = std::slice::#slice_from (#ident as * #const_token #mutability #sub_ty, #size_ident as usize);).into()
                    ).unwrap());
                    ty
                }

                Type::Ptr(TypePtr {
                    mutability: None,
                    elem,
                    ..
                }) if is_ident(&elem, "c_char") => {
                    let ty = syn::parse(quote!(&str).into()).unwrap();
                    conversions.push(
                        syn::parse(
                            quote!(let #new_ident = std::ffi::CStr::from_ptr(#ident).to_str().unwrap();)
                                .into(),
                        )
                        .unwrap(),
                    );
                    ty
                }

                Type::Ptr(TypePtr {
                    mutability, elem, ..
                }) => {
                    let ty = syn::parse(quote!(Option<& #mutability #elem>).into()).unwrap();
                    if let Type::Path(path) = *elem {
                        if let Some(ident) = path.path.get_ident() {
                            reexport_types.insert(ident.to_string());
                        }
                    }

                    let ref_from: Ident = syn::parse(
                        if mutability.is_none() {
                            quote!(as_ref)
                        } else {
                            quote!(as_mut)
                        }
                        .into(),
                    )
                    .unwrap();

                    conversions.push(
                        syn::parse(quote!(let #new_ident = #ident . #ref_from ();).into()).unwrap(),
                    );

                    ty
                }

                // fuse_fill_dir is a typedef for an unsafe function pointer.
                // I'd like to parse it automatically, just like all the other function pointers we deal with
                // but I can't find a way of extracting the signature of the function pointer from the typedef.
                //
                // Here's the signature we are assuming:
                // pub type fuse_fill_dir_t = Option<unsafe extern "C" fn(buf: *mut c_void, name: *const c_char, stbuf: *const stat, off: off_t, flags: u32) -> c_int>;
                // Type::Path(path) if is_ident(&Type::Path(path.clone()), "fuse_fill_dir_t") => {
                //     conversions.push(syn::parse(quote! {
                //         let #new_ident = {
                //             let #ident = #ident.unwrap();
                //             move |buf: Option<&mut std::ffi::c_void>, name: &str, stat: &stat, off: off_t, flags: u32| {
                //                 let mut buf = buf.map(|buf| buf as *mut std::ffi::c_void).unwrap_or(0 as *mut std::ffi::c_void);
                //                 let name = std::ffi::CString::new(name).unwrap();
                //                 let stat = stat as *const stat;
                //                 #ident (buf, name.as_ptr(), stat, off, flags)
                //             }
                //         };
                //     }.into()).unwrap());

                //     syn::parse(
                //         quote!(

                //             impl Fn(
                //                 Option<&mut std::ffi::c_void>,
                //                 &str,
                //                 &stat,
                //                 off_t,
                //                 u32,
                //             ) -> std::os::raw::c_int
                //         )
                //         .into(),
                //     )
                //     .unwrap()
                // }
                Type::Path(path) => {
                    if let Some(ident) = path.path.get_ident() {
                        reexport_types.insert(ident.to_string());
                    }
                    conversions.push(syn::parse(quote!(let #new_ident = #ident;).into()).unwrap());
                    Type::Path(path)
                }

                ty => {
                    conversions.push(syn::parse(quote!(let #new_ident = #ident;).into()).unwrap());
                    ty
                }
            };

            new_inputs.push(syn::parse(quote!(#ident: #new_ty).into()).unwrap());
        }

        Self {
            new_inputs,
            unconverted_call,
            converted_call,
            reexport_types,
            conversion: conversions.into_iter().collect(),
        }
    }
}

#[proc_macro_attribute]
pub fn fuse_operations(attr: TokenStream, item: TokenStream) -> TokenStream {
    let blacklist = Punctuated::<Ident, Comma>::parse_terminated
        .parse(attr)
        .map(|p| p.into_iter().collect::<Vec<_>>())
        .unwrap();

    let out: TokenStream2 = item.clone().into();
    let tokens = parse_macro_input!(item as ItemStruct);

    let fields = match tokens.fields {
        Fields::Named(fields) => fields.named,
        _ => unimplemented!(),
    };

    let mut raw_unthreaded_fns = TokenStream2::new();
    let mut raw_threaded_fns = TokenStream2::new();

    let mut raw_trait_fn_sigs = TokenStream2::new();

    let mut unthreaded_fns = TokenStream2::new();
    let mut threaded_fns = TokenStream2::new();

    let mut op_assignments: Vec<Stmt> = vec![];
    let mut all_reexport_types = HashSet::new();

    for field in fields {
        let name = field.ident.unwrap();

        if blacklist.contains(&name) {
            continue;
        }

        let ty_path = match field.ty {
            Type::Path(path) => path,
            _ => continue,
        };

        let ty = ty_path.path.segments.last().unwrap();
        if ty.ident != "Option" {
            continue;
        }

        let args = match &ty.arguments {
            PathArguments::AngleBracketed(args) => args,
            _ => continue,
        };

        let TypeBareFn {
            unsafety,
            abi,
            inputs,
            variadic,
            output,
            ..
        } = match args.args.first().unwrap() {
            GenericArgument::Type(Type::BareFn(ty)) => ty,
            _ => continue,
        };

        if variadic.is_some()
            || !matches!(output, ReturnType::Type(_, ty)
                if is_ident(ty, "c_int")
            )
        {
            continue;
        }

        let UnsafeFnConvert {
            new_inputs,
            unconverted_call,
            converted_call,
            reexport_types,
            conversion,
        } = UnsafeFnConvert::new(inputs.clone());

        all_reexport_types.extend(reexport_types);

        let dummy_private_data_ident = gen_ident("dummy_private");
        let private_data_ident = gen_ident("private");
        let dummy_fs_ident = gen_ident("dummy_fs");
        let out_ident = gen_ident("out");

        let fuse_fs_name: TokenStream2 = format!("crate::fuse_fs_{name}").parse().unwrap();

        unthreaded_fns.extend([quote! {
            fn #name (&mut self, #new_inputs) -> anyhow::Result<i32> {
                Err(std::io::Error::from_raw_os_error(38).into())
            }
        }]);
        threaded_fns.extend([quote! {
            fn #name (&self, #new_inputs) -> anyhow::Result<i32> {
                Err(std::io::Error::from_raw_os_error(38).into())
            }
        }]);

        raw_trait_fn_sigs.extend([quote! {
            #unsafety #abi fn #name (#inputs) #output;
        }]);

        for (stream, convert_ptr) in [
            (&mut raw_threaded_fns, quote!(as_ref)),
            (&mut raw_unthreaded_fns, quote!(as_mut)),
        ] {
            stream.extend([quote! {
                #unsafety #abi fn #name (#inputs) #output {
                    #conversion

                    let mut #private_data_ident = UserData::<Self>::from_raw((*fuse_get_context()).private_data);

                    let #out_ident = Self::#name(
                        #private_data_ident.this.#convert_ptr().expect("Private data mangled"),
                        #converted_call
                    );

                    let #out_ident = match #out_ident {
                        Ok(o) => o,
                        Err(e) => {
                            if let Some(err) = e.downcast_ref::<std::io::Error>() {
                                match err.raw_os_error() {
                                    Some(os) => -os,
                                    None => {
                                        eprintln!(
                                            "Unrecognized error in {}: {:?}",
                                            stringify!(#name),
                                            err
                                        );
                                        -131
                                    }
                                }
                            } else if let Some(&err) = e.downcast_ref::<nix::errno::Errno>() {
                                -(err as i32)
                            } else {
                                eprintln!(
                                    "Unrecognized error in {}: {:?}",
                                    stringify!(#name),
                                    e
                                );
                                -131
                            }
                        }
                    };

                    if #out_ident == -38 {
                        let mut #dummy_private_data_ident = {
                            let mut ops = #private_data_ident.ops.clone();
                            ops.#name = None;
                            UserData::new(ops, #private_data_ident.this)
                        };

                        let #dummy_fs_ident = crate::fuse_fs_new(
                            &#dummy_private_data_ident.ops as *const _,
                            std::mem::size_of::<crate::fuse_operations>(),
                            &mut #dummy_private_data_ident as *mut _ as *mut std::ffi::c_void,
                        );

                        let out = #fuse_fs_name(#dummy_fs_ident, #unconverted_call);

                        crate::fuse_fs_destroy(#dummy_fs_ident);
                        out
                    } else {
                        #out_ident
                    }
                }
            }]);
        }

        op_assignments
            .push(syn::parse(quote!(operations.#name = Some(Self::#name);).into()).unwrap());
    }

    let op_assignments: Punctuated<Stmt, Semi> = op_assignments.into_iter().collect();

    let reexport_list: Punctuated<Type, Comma> = all_reexport_types
        .into_iter()
        .filter_map(|s| {
            (!PRIMITIVE_IDENTS.contains(&s.as_ref()))
                .then(|| syn::parse::<Type>(s.parse().unwrap()).unwrap())
        })
        .collect();

    quote! {
        #[allow(unused_variables)]
        pub trait UnthreadedFileSystem: Sized {
            #unthreaded_fns
        }
        pub trait FileSystem: Sized {
            #threaded_fns
        }

        pub trait FileSystemRaw<const UNTHREADED: bool> {
            #raw_trait_fn_sigs
        }
        impl<F: UnthreadedFileSystem> FileSystemRaw<true> for F {
            #raw_unthreaded_fns
        }
        impl<F: FileSystem + Send + Sync> FileSystemRaw<false> for F {
            #raw_threaded_fns
        }

        pub trait FuseMain<const UNTHREADED: bool>: FileSystemRaw<UNTHREADED> + 'static {
            fn run(self, fuse_args: &[&str]) -> Result<(), i32>;
        }

        struct UserData<T> {
            ops: crate::fuse_operations,
            this: *mut T,
        }

        impl<T> UserData<T> {
            fn new(ops: crate::fuse_operations, this: *mut T) -> Self {
                Self {
                    ops,
                    this,
                }
            }

            unsafe fn from_raw<'a>(raw: *mut std::ffi::c_void) -> &'a Self {
                (raw as *const Self).as_ref().expect("Mangled UserData")
            }
        }

        impl<const UNTHREADED: bool, F: FileSystemRaw<UNTHREADED> + 'static> FuseMain<UNTHREADED> for F {
            fn run(self, fuse_args: &[&str]) -> Result<(), i32> {
                let mut operations = crate::fuse_operations::default();
                #op_assignments

                let mut this = self;
                let mut user_data = UserData::new(
                    operations.clone(),
                    &mut this as *mut _,
                );

                let mut args_owned: std::vec::Vec<_> = fuse_args.into_iter().map(|s| std::ffi::CString::new(*s).unwrap()).collect();

                if UNTHREADED {
                    args_owned.push(std::ffi::CString::new("-s").unwrap());
                }

                let mut args: std::vec::Vec<_> = args_owned.iter_mut().map(|cs| cs.as_ptr()).collect();

                let out = unsafe {
                    crate::fuse_main_real(
                        args.len() as i32,
                        args.as_mut_ptr() as *mut *mut std::os::raw::c_char,
                        &operations as *const crate::fuse_operations,
                        std::mem::size_of::<crate::fuse_operations>(),
                        &mut user_data as *mut _ as *mut std::ffi::c_void,
                    )
                };

                match out {
                    0 => Ok(()),
                    e => Err(e),
                }
            }
        }

        pub mod prelude {
            pub use crate::{
                UnthreadedFileSystem,
                FileSystem,
                FuseMain,
                #reexport_list
            };
        }

        #out
    }.into()
}
