// Copyright 2019 Contributors to the Parsec project.
// SPDX-License-Identifier: Apache-2.0
#![deny(
    nonstandard_style,
    const_err,
    dead_code,
    improper_ctypes,
    non_shorthand_field_patterns,
    no_mangle_generic_items,
    overflowing_literals,
    path_statements,
    patterns_in_fns_without_body,
    private_in_public,
    unconditional_recursion,
    unused,
    unused_allocation,
    unused_comparisons,
    unused_parens,
    while_true,
    missing_debug_implementations,
    //TODO: activate this!
    //missing_docs,
    trivial_casts,
    trivial_numeric_casts,
    unused_extern_crates,
    unused_import_braces,
    unused_qualifications,
    unused_results,
    missing_copy_implementations
)]
//! # TSS 2.0 Rust Wrapper over Enhanced System API
//! This crate exposes the functionality of the TCG Software Stack Enhanced System API to
//! Rust developers, both directly through FFI bindings and through more Rust-tailored interfaces
//! at varying levels of abstraction.
//! At the moment, the abstracted functionality focuses on creating signing and encryption RSA
//! keys, as well as signing and verifying signatures.
//! Only platforms based on processors with a word size of at least 16 bits are supported.
//!
//! The crate is expected to successfully compile and run using the nightly compiler and any other
//! Rust compiler since 1.38.0.
//!
//! # Disclaimer
//!
//! The current version of the API does not offer any security or code safety guarantees as it has
//! not been tested to a desired level of confidence.
//! The implementation that is provided is suitable for exploratory testing and experimentation only.
//! This test implementation does not offer any tangible security benefits and therefore is not
//! suitable for use in production.
//! Contributions from the developer community are welcome. Please refer to the contribution guidelines.
//!
//! # Code structure
//! The modules comprising the crate expose the following functionalities:
//! * lib/root module - exposes the `Context` structure, the most basic abstraction over the
//! ESAPI, on top of which all other abstraction layers are implemented.
//! * utils - exposes Rust-native versions and/or builders for (some of) the structures defined in
//! the TSS 2.0 specification; it also offers convenience methods for generating very specific
//! parameter structures for use in certain operations.
//! * response_code - implements error code parsing for the formats defined in the TSS spec and
//! exposes it along with wrapper-specific error types.
//! * abstraction - intended to offer abstracted interfaces that focus on providing different
//! kinds of user experience to the developers; at the moment the only implementation allows for a
//! resource-handle-free coding experience by working soloely with object contexts.
//! * tss2_esys - exposes raw FFI bindings to the Enhanced System API.
//! * constants - exposes constants that were ported to Rust manually as bindgen does not support
//! converting them yet.
//!
//! # Notes on code safety:
//! * thread safety is ensured by the required mutability of the `Context` structure within the
//! methods implemented on it; thus, in an otherwise safe app commands cannot be dispatched in
//! parallel for the same context; whether multithreading with multiple context objects is possible
//! depends on the TCTI used and this is the responsability of the crate client to establish.
//! * the `unsafe` keyword is used to denote methods that could panic, crash or cause undefined
//! behaviour. Whenever this is the case, the properties that need to be checked against
//! parameters before passing them in will be stated in the documentation of the method.
//! * `unsafe` blocks within this crate need to be documented through code comments if they
//! are not covered by the points of trust described here.
//! * the TSS2.0 library that this crate links to is trusted to return consistent values and to
//! not crash or lead to undefined behaviour when presented with valid arguments.
//! * the `Mbox` crate is trusted to perform operations safely on the pointers provided to it, if
//! the pointers are trusted to be valid.
//! * methods not marked `unsafe` are trusted to behave safely, potentially returning appropriate
//! erorr messages when encountering any problems.
//! * whenever `unwrap`, `expect`, `panic` or derivatives of these are used, they need to be
//! thoroughly documented and justified - preferably `unwrap` and `expect` should *never* fail
//! during normal operation.
//! * these rules can be broken in test-only code and in tests.

#![allow(dead_code)]

#[allow(
    non_snake_case,
    non_camel_case_types,
    non_upper_case_globals,
    dead_code
)]
#[allow(clippy::all)]
#[allow(clippy::unseparated_literal_suffix)]
// There is an issue where long double become u128 in extern blocks. Check this issue:
// https://github.com/rust-lang/rust-bindgen/issues/1549
#[allow(improper_ctypes, missing_debug_implementations, trivial_casts)]
pub mod tss2_esys {
    #[cfg(not(feature = "docs"))]
    include!(concat!(env!("OUT_DIR"), "/tss2_esys_bindings.rs"));

    #[cfg(feature = "docs")]
    include!(concat!(env!("CARGO_MANIFEST_DIR"), "/doc_bindings.rs"));
}
pub mod abstraction;
#[allow(
    non_snake_case,
    non_camel_case_types,
    non_upper_case_globals,
    dead_code
)]
#[allow(clippy::all)]
pub mod constants;
pub mod response_code;
pub mod utils;

pub use abstraction::transient::TransientKeyContext;
use log::{error, info};
use mbox::MBox;
use response_code::Result;
use response_code::{Error, WrapperErrorKind as ErrorKind};
use std::collections::HashSet;
use std::convert::{TryFrom, TryInto};
use std::ffi::CString;
use std::ptr::{null, null_mut};
use tss2_esys::*;
use utils::tcti::Tcti;
use utils::{
    algorithm_specifiers::HashingAlgorithm, tickets::HashcheckTicket, Hierarchy, PcrData,
    PcrSelections, PublicParmsUnion, Signature, TpmaSession, TpmaSessionBuilder, TpmsContext,
};

#[macro_use]
macro_rules! wrap_buffer {
    ($buf:expr, $buf_type:ty, $buf_size:expr) => {{
        if $buf.len() > $buf_size {
            return Err(Error::local_error(ErrorKind::WrongParamSize));
        }
        let mut buffer = [0_u8; $buf_size];
        buffer[..$buf.len()].clone_from_slice(&$buf[..$buf.len()]);
        let mut buf_struct: $buf_type = Default::default();
        buf_struct.size = $buf.len().try_into().unwrap(); // should not fail since the length is checked above
        buf_struct.buffer = buffer;
        buf_struct
    }};
}

/// Safe abstraction over an ESYS_CONTEXT.
///
/// Serves as a low-level abstraction interface to the TPM, providing a thin wrapper around the
/// `unsafe` FFI calls. It is meant for more advanced uses of the TSS where control over all
/// parameters is necessary or important.
///
/// The methods it exposes take the parameters advertised by the specification, with some of the
/// parameters being passed as generated by `bindgen` and others in a more convenient/Rust-efficient
/// way.
///
/// The context also keeps track of all object allocated and deallocated through it and, before
/// being dropped, will attempt to close all outstanding handles. However, care must be taken by
/// the client to not exceed the maximum number of slots available from the RM.
///
/// Code safety-wise, the methods should cover the two kinds of problems that might arise:
/// * in terms of memory safety, all parameters passed down to the TSS are verified and the library
/// stack is then trusted to provide back valid outputs
/// * in terms of thread safety, all methods require a mutable reference to the context object,
/// ensuring that no two threads can use the context at the same time for an operation (barring use
/// of `unsafe` constructs on the client side)
/// More testing and verification will be added to ensure this.
///
/// For most methods, if the wrapped TSS call fails and returns a non-zero `TPM2_RC`, a
/// corresponding `Tss2ResponseCode` will be created and returned as an `Error`. Wherever this is
/// not the case or additional error types can be returned, the method definition should mention
/// it.
#[derive(Debug)]
pub struct Context {
    /// Handle for the ESYS context object owned through an Mbox.
    /// Wrapping the handle in an optional Mbox is done to allow the `Context` to be closed properly when the `Context` structure is dropped.
    esys_context: Option<MBox<ESYS_CONTEXT>>,
    sessions: (ESYS_TR, ESYS_TR, ESYS_TR),
    /// TCTI context handle associated with the ESYS context.
    /// As with the ESYS context, an optional Mbox wrapper allows the context to be deallocated.
    tcti_context: Option<MBox<TSS2_TCTI_CONTEXT>>,
    /// A set of currently open object handles that should be flushed before closing the context.
    open_handles: HashSet<ESYS_TR>,
}

impl Context {
    /// Create a new ESYS context based on the desired TCTI
    ///
    /// # Safety
    /// * the client is responsible for ensuring that the context can be initialized safely,
    /// threading-wise
    ///
    /// # Errors
    /// * if either `Tss2_TctiLdr_Initiialize` or `Esys_Initialize` fail, a corresponding
    /// Tss2ResponseCode will be returned
    pub unsafe fn new(tcti: Tcti) -> Result<Self> {
        let mut esys_context = null_mut();
        let mut tcti_context = null_mut();

        let tcti_name_conf = CString::try_from(tcti)?; // should never panic

        let ret = Tss2_TctiLdr_Initialize(tcti_name_conf.as_ptr(), &mut tcti_context);
        let ret = Error::from_tss_rc(ret);
        if !ret.is_success() {
            error!("Error when creating a TCTI context: {}.", ret);
            return Err(ret);
        }
        let mut tcti_context = Some(MBox::from_raw(tcti_context));

        let ret = Esys_Initialize(
            &mut esys_context,
            tcti_context.as_mut().unwrap().as_mut_ptr(), // will not panic as per how tcti_context is initialised
            null_mut(),
        );
        let ret = Error::from_tss_rc(ret);

        if ret.is_success() {
            let esys_context = Some(MBox::from_raw(esys_context));
            let context = Context {
                esys_context,
                sessions: (ESYS_TR_NONE, ESYS_TR_NONE, ESYS_TR_NONE),
                tcti_context,
                open_handles: HashSet::new(),
            };
            Ok(context)
        } else {
            error!("Error when creating a new context: {}.", ret);
            Err(ret)
        }
    }

    /// Start new authentication session and return the handle.
    ///
    /// The caller nonce is passed as a slice and converted by the method in a TSS digest
    /// structure.
    ///
    /// # Constraints
    /// * nonce must be at most 64 elements long
    ///
    /// # Errors
    /// * if the `nonce` is larger than allowed, a `WrongSizeParam` wrapper error is returned
    // TODO: Fix when compacting the arguments into a struct
    #[allow(clippy::too_many_arguments)]
    pub fn start_auth_session(
        &mut self,
        tpm_key: ESYS_TR,
        bind: ESYS_TR,
        nonce: &[u8],
        session_type: TPM2_SE,
        symmetric: TPMT_SYM_DEF,
        auth_hash: TPMI_ALG_HASH,
    ) -> Result<ESYS_TR> {
        let nonce_caller = wrap_buffer!(nonce, TPM2B_NONCE, 64);
        let mut sess = ESYS_TR_NONE;

        let ret = unsafe {
            Esys_StartAuthSession(
                self.mut_context(),
                tpm_key,
                bind,
                self.sessions.0,
                self.sessions.1,
                self.sessions.2,
                if nonce.is_empty() {
                    null()
                } else {
                    &nonce_caller
                },
                session_type,
                &symmetric,
                auth_hash,
                &mut sess,
            )
        };

        let ret = Error::from_tss_rc(ret);
        if ret.is_success() {
            let _ = self.open_handles.insert(sess);
            Ok(sess)
        } else {
            error!("Error when creating a session: {}.", ret);
            Err(ret)
        }
    }

    pub fn set_sessions(&mut self, session_handles: (ESYS_TR, ESYS_TR, ESYS_TR)) {
        self.sessions = session_handles;
    }

    pub fn sessions(&self) -> (ESYS_TR, ESYS_TR, ESYS_TR) {
        self.sessions
    }

    /// Create a primary key and return the handle.
    ///
    /// The authentication value, initial data, outside info and creation PCRs are passed as slices
    /// which are then converted by the method into TSS native structures.
    ///
    /// # Constraints
    /// * `outside_info` must be at most 64 elements long
    /// * `creation_pcrs` must be at most 16 elements long
    /// * `auth_value` must be at most 64 elements long
    /// * `initial_data` must be at most 256 elements long
    ///
    /// # Errors
    /// * if either of the slices is larger than the maximum size of the native objects, a
    /// `WrongParamSize` wrapper error is returned
    // TODO: Fix when compacting the arguments into a struct
    #[allow(clippy::too_many_arguments)]
    pub fn create_primary_key(
        &mut self,
        primary_handle: ESYS_TR,
        public: &TPM2B_PUBLIC,
        auth_value: &[u8],
        initial_data: &[u8],
        outside_info: &[u8],
        creation_pcrs: &[TPMS_PCR_SELECTION],
    ) -> Result<ESYS_TR> {
        let sensitive_create = TPM2B_SENSITIVE_CREATE {
            size: std::mem::size_of::<TPMS_SENSITIVE_CREATE>()
                .try_into()
                .unwrap(), // will not fail on targets of at least 16 bits
            sensitive: TPMS_SENSITIVE_CREATE {
                userAuth: wrap_buffer!(auth_value, TPM2B_AUTH, 64),
                data: wrap_buffer!(initial_data, TPM2B_SENSITIVE_DATA, 256),
            },
        };
        let outside_info = wrap_buffer!(outside_info, TPM2B_DATA, 64);

        if creation_pcrs.len() > 16 {
            return Err(Error::local_error(ErrorKind::WrongParamSize));
        }

        let mut creation_pcrs_buffer = [Default::default(); 16];
        creation_pcrs_buffer[..creation_pcrs.len()]
            .clone_from_slice(&creation_pcrs[..creation_pcrs.len()]);
        let creation_pcrs = TPML_PCR_SELECTION {
            count: creation_pcrs.len().try_into().unwrap(), // will not fail given the len checks above
            pcrSelections: creation_pcrs_buffer,
        };

        let mut outpublic = null_mut();
        let mut creation_data = null_mut();
        let mut creation_hash = null_mut();
        let mut creation_ticket = null_mut();
        let mut prim_key_handle = ESYS_TR_NONE;

        let ret = unsafe {
            Esys_CreatePrimary(
                self.mut_context(),
                primary_handle,
                self.sessions.0,
                self.sessions.1,
                self.sessions.2,
                &sensitive_create,
                public,
                &outside_info,
                &creation_pcrs,
                &mut prim_key_handle,
                &mut outpublic,
                &mut creation_data,
                &mut creation_hash,
                &mut creation_ticket,
            )
        };
        let ret = Error::from_tss_rc(ret);

        if ret.is_success() {
            unsafe {
                let _ = MBox::from_raw(outpublic);
                let _ = MBox::from_raw(creation_data);
                let _ = MBox::from_raw(creation_hash);
                let _ = MBox::from_raw(creation_ticket);
            }
            let _ = self.open_handles.insert(prim_key_handle);
            Ok(prim_key_handle)
        } else {
            error!("Error in creating primary key: {}.", ret);
            Err(ret)
        }
    }

    /// Create a key and return the handle.
    ///
    /// The authentication value, initial data, outside info and creation PCRs are passed as slices
    /// which are then converted by the method into TSS native structures.
    ///
    /// # Constraints
    /// * `outside_info` must be at most 64 elements long
    /// * `creation_pcrs` must be at most 16 elements long
    /// * `auth_value` must be at most 64 elements long
    /// * `initial_data` must be at most 256 elements long
    ///
    /// # Errors
    /// * if either of the slices is larger than the maximum size of the native objects, a
    /// `WrongParamSize` wrapper error is returned
    // TODO: Fix when compacting the arguments into a struct
    #[allow(clippy::too_many_arguments)]
    pub fn create_key(
        &mut self,
        parent_handle: ESYS_TR,
        public: &TPM2B_PUBLIC,
        auth_value: &[u8],
        initial_data: &[u8],
        outside_info: &[u8],
        creation_pcrs: &[TPMS_PCR_SELECTION],
    ) -> Result<(TPM2B_PRIVATE, TPM2B_PUBLIC)> {
        let sensitive_create = TPM2B_SENSITIVE_CREATE {
            size: std::mem::size_of::<TPMS_SENSITIVE_CREATE>()
                .try_into()
                .unwrap(), // will not fail on targets of at least 16 bits
            sensitive: TPMS_SENSITIVE_CREATE {
                userAuth: wrap_buffer!(auth_value, TPM2B_AUTH, 64),
                data: wrap_buffer!(initial_data, TPM2B_SENSITIVE_DATA, 256),
            },
        };

        let outside_info = wrap_buffer!(outside_info, TPM2B_DATA, 64);

        if creation_pcrs.len() > 16 {
            return Err(Error::local_error(ErrorKind::WrongParamSize));
        }
        let mut creation_pcrs_buffer = [Default::default(); 16];
        creation_pcrs_buffer[..creation_pcrs.len()]
            .clone_from_slice(&creation_pcrs[..creation_pcrs.len()]);
        let creation_pcrs = TPML_PCR_SELECTION {
            count: creation_pcrs.len().try_into().unwrap(), // will not fail given the len checks above
            pcrSelections: creation_pcrs_buffer,
        };

        let mut outpublic = null_mut();
        let mut outprivate = null_mut();
        let mut creation_data = null_mut();
        let mut digest = null_mut();
        let mut creation = null_mut();

        let ret = unsafe {
            Esys_Create(
                self.mut_context(),
                parent_handle,
                self.sessions.0,
                self.sessions.1,
                self.sessions.2,
                &sensitive_create,
                public,
                &outside_info,
                &creation_pcrs,
                &mut outprivate,
                &mut outpublic,
                &mut creation_data,
                &mut digest,
                &mut creation,
            )
        };
        let ret = Error::from_tss_rc(ret);

        if ret.is_success() {
            let outprivate = unsafe { MBox::from_raw(outprivate) };
            let outpublic = unsafe { MBox::from_raw(outpublic) };
            unsafe {
                let _ = MBox::from_raw(creation_data);
                let _ = MBox::from_raw(digest);
                let _ = MBox::from_raw(creation);
            }
            Ok((*outprivate, *outpublic))
        } else {
            error!("Error in creating derived key: {}.", ret);
            Err(ret)
        }
    }

    /// Load a previously generated key back into the TPM and return its new handle.
    pub fn load(
        &mut self,
        parent_handle: ESYS_TR,
        private: TPM2B_PRIVATE,
        public: TPM2B_PUBLIC,
    ) -> Result<ESYS_TR> {
        let mut handle = ESYS_TR_NONE;
        let ret = unsafe {
            Esys_Load(
                self.mut_context(),
                parent_handle,
                self.sessions.0,
                self.sessions.1,
                self.sessions.2,
                &private,
                &public,
                &mut handle,
            )
        };
        let ret = Error::from_tss_rc(ret);

        if ret.is_success() {
            let _ = self.open_handles.insert(handle);
            Ok(handle)
        } else {
            error!("Error in loading: {}.", ret);
            Err(ret)
        }
    }

    /// Sign a digest with a key present in the TPM and return the signature.
    ///
    /// The digest is passed as a slice, converted by the method to a TSS digest structure.
    ///
    /// # Constraints
    /// * `digest` must be at most 64 elements long
    ///
    /// # Errors
    /// * if the digest provided is too long, a `WrongParamSize` wrapper error will be returned
    pub fn sign(
        &mut self,
        key_handle: ESYS_TR,
        digest: &[u8],
        scheme: TPMT_SIG_SCHEME,
        validation: &TPMT_TK_HASHCHECK,
    ) -> Result<Signature> {
        let mut signature = null_mut();
        let digest = wrap_buffer!(digest, TPM2B_DIGEST, 64);
        let ret = unsafe {
            Esys_Sign(
                self.mut_context(),
                key_handle,
                self.sessions.0,
                self.sessions.1,
                self.sessions.2,
                &digest,
                &scheme,
                validation,
                &mut signature,
            )
        };
        let ret = Error::from_tss_rc(ret);

        if ret.is_success() {
            let signature = unsafe { MBox::from_raw(signature) };
            Ok(unsafe { Signature::try_from(*signature)? })
        } else {
            error!("Error in loading: {}.", ret);
            Err(ret)
        }
    }

    /// Verify if a signature was generated by signing a given digest with a key in the TPM.
    ///
    /// The digest is passed as a sliice and converted by the method to a TSS digest structure.
    ///
    /// # Constraints
    /// * `digest` must be at most 64 elements long
    ///
    /// # Errors
    /// * if the digest provided is too long, a `WrongParamSize` wrapper error will be returned
    pub fn verify_signature(
        &mut self,
        key_handle: ESYS_TR,
        digest: &[u8],
        signature: &TPMT_SIGNATURE,
    ) -> Result<TPMT_TK_VERIFIED> {
        let mut validation = null_mut();
        let digest = wrap_buffer!(digest, TPM2B_DIGEST, 64);
        let ret = unsafe {
            Esys_VerifySignature(
                self.mut_context(),
                key_handle,
                self.sessions.0,
                self.sessions.1,
                self.sessions.2,
                &digest,
                signature,
                &mut validation,
            )
        };
        let ret = Error::from_tss_rc(ret);

        if ret.is_success() {
            let validation = unsafe { MBox::from_raw(validation) };
            Ok(*validation)
        } else {
            error!("Error in loading: {}.", ret);
            Err(ret)
        }
    }

    /// Load an external key into the TPM and return its new handle.
    pub fn load_external(
        &mut self,
        private: &TPM2B_SENSITIVE,
        public: &TPM2B_PUBLIC,
        hierarchy: Hierarchy,
    ) -> Result<ESYS_TR> {
        let mut key_handle = ESYS_TR_NONE;
        let ret = unsafe {
            Esys_LoadExternal(
                self.mut_context(),
                self.sessions.0,
                self.sessions.1,
                self.sessions.2,
                private,
                public,
                hierarchy.rh(),
                &mut key_handle,
            )
        };

        let ret = Error::from_tss_rc(ret);

        if ret.is_success() {
            let _ = self.open_handles.insert(key_handle);
            Ok(key_handle)
        } else {
            error!("Error in loading: {}.", ret);
            Err(ret)
        }
    }

    /// Load the public part of an external key and return its new handle.
    pub fn load_external_public(
        &mut self,
        public: &TPM2B_PUBLIC,
        hierarchy: Hierarchy,
    ) -> Result<ESYS_TR> {
        let mut key_handle = ESYS_TR_NONE;
        let ret = unsafe {
            Esys_LoadExternal(
                self.mut_context(),
                self.sessions.0,
                self.sessions.1,
                self.sessions.2,
                null(),
                public,
                hierarchy.rh(),
                &mut key_handle,
            )
        };

        let ret = Error::from_tss_rc(ret);

        if ret.is_success() {
            let _ = self.open_handles.insert(key_handle);
            Ok(key_handle)
        } else {
            error!("Error in loading: {}.", ret);
            Err(ret)
        }
    }

    /// Read the public part of a key currently in the TPM and return it.
    pub fn read_public(&mut self, key_handle: ESYS_TR) -> Result<TPM2B_PUBLIC> {
        let mut public = null_mut();
        let mut name = null_mut();
        let mut qualified_name = null_mut();
        let ret = unsafe {
            Esys_ReadPublic(
                self.mut_context(),
                key_handle,
                self.sessions.0,
                self.sessions.1,
                self.sessions.2,
                &mut public,
                &mut name,
                &mut qualified_name,
            )
        };
        let ret = Error::from_tss_rc(ret);

        if ret.is_success() {
            unsafe {
                let _ = MBox::from_raw(name);
                let _ = MBox::from_raw(qualified_name);
            }
            let public = unsafe { MBox::<TPM2B_PUBLIC>::from_raw(public) };
            Ok(*public)
        } else {
            error!("Error in loading: {}.", ret);
            Err(ret)
        }
    }

    /// Flush the context of an object from the TPM.
    pub fn flush_context(&mut self, handle: ESYS_TR) -> Result<()> {
        let ret = unsafe { Esys_FlushContext(self.mut_context(), handle) };
        let ret = Error::from_tss_rc(ret);
        if ret.is_success() {
            let _ = self.open_handles.remove(&handle);
            Ok(())
        } else {
            error!("Error in flushing context: {}.", ret);
            Err(ret)
        }
    }

    /// Save the context of an object from the TPM and return it.
    ///
    /// # Errors
    /// * if conversion from `TPMS_CONTEXT` to `TpmsContext` fails, a `WrongParamSize` error will
    /// be returned
    pub fn context_save(&mut self, handle: ESYS_TR) -> Result<TpmsContext> {
        let mut context = null_mut();
        let ret = unsafe { Esys_ContextSave(self.mut_context(), handle, &mut context) };

        let ret = Error::from_tss_rc(ret);
        if ret.is_success() {
            let context = unsafe { MBox::<TPMS_CONTEXT>::from_raw(context) };
            Ok((*context).try_into()?)
        } else {
            error!("Error in saving context: {}.", ret);
            Err(ret)
        }
    }

    /// Load a previously saved context into the TPM and return the object handle.
    ///
    /// # Errors
    /// * if conversion from `TpmsContext` to the native `TPMS_CONTEXT` fails, a `WrongParamSize`
    /// error will be returned
    pub fn context_load(&mut self, context: TpmsContext) -> Result<ESYS_TR> {
        let mut handle = ESYS_TR_NONE;
        let ret = unsafe {
            Esys_ContextLoad(
                self.mut_context(),
                &TPMS_CONTEXT::try_from(context)?,
                &mut handle,
            )
        };

        let ret = Error::from_tss_rc(ret);
        if ret.is_success() {
            let _ = self.open_handles.insert(handle);
            Ok(handle)
        } else {
            error!("Error in loading context: {}.", ret);
            Err(ret)
        }
    }

    /// Reads the value of a PCR slot associated with
    /// a specific hashing algorithm
    ///
    /// # Constraints
    /// * If the selection contains more pcr values then 16 (number of
    /// elements in TPML_DIGEST). Then not all values will be read. The
    /// Selection in the return value will indicate what values that have
    /// been read.
    ///
    /// # Errors
    /// * Several different errors can occur if conversion of return
    ///   data fails.
    pub fn pcr_read(
        &mut self,
        pcr_selections: PcrSelections,
    ) -> Result<(u32, PcrSelections, PcrData)> {
        let mut pcr_update_counter: u32 = 0;
        let mut tpml_pcr_selection_out_ptr = null_mut();
        let mut tpml_digest_ptr = null_mut();
        let ret = unsafe {
            Esys_PCR_Read(
                self.mut_context(),
                self.sessions.0,
                self.sessions.1,
                self.sessions.2,
                &pcr_selections.into(),
                &mut pcr_update_counter,
                &mut tpml_pcr_selection_out_ptr,
                &mut tpml_digest_ptr,
            )
        };
        let ret = Error::from_tss_rc(ret);

        if ret.is_success() {
            let tpml_pcr_selection_out =
                unsafe { MBox::<TPML_PCR_SELECTION>::from_raw(tpml_pcr_selection_out_ptr) };
            let tpml_digest = unsafe { MBox::<TPML_DIGEST>::from_raw(tpml_digest_ptr) };
            Ok((
                pcr_update_counter,
                PcrSelections::try_from(*tpml_pcr_selection_out)?,
                PcrData::new(tpml_pcr_selection_out.as_ref(), tpml_digest.as_ref())?,
            ))
        } else {
            error!("Error in creating derived key: {}.", ret);
            Err(ret)
        }
    }

    /// Generate a quote on the selected PCRs
    ///
    /// # Constraints
    /// * `qualifying_data` must be at most 64 elements long
    ///
    /// # Errors
    /// * if the qualifying data provided is too long, a `WrongParamSize` wrapper error will be returned
    pub fn quote(
        &mut self,
        signing_key_handle: ESYS_TR,
        qualifying_data: &[u8],
        signing_scheme: TPMT_SIG_SCHEME,
        pcr_selection: PcrSelections,
    ) -> Result<(TPM2B_ATTEST, Signature)> {
        let mut quoted = null_mut();
        let mut signature = null_mut();
        let qualifying_data = wrap_buffer!(qualifying_data, TPM2B_DATA, 64);

        let ret = unsafe {
            Esys_Quote(
                self.mut_context(),
                signing_key_handle,
                self.sessions.0,
                self.sessions.1,
                self.sessions.2,
                &qualifying_data,
                &signing_scheme,
                &pcr_selection.into(),
                &mut quoted,
                &mut signature,
            )
        };
        let ret = Error::from_tss_rc(ret);

        if ret.is_success() {
            let quoted = unsafe { MBox::<TPM2B_ATTEST>::from_raw(quoted) };
            let signature = unsafe { MBox::from_raw(signature) };
            Ok((*quoted, unsafe { Signature::try_from(*signature)? }))
        } else {
            error!("Error in quoting PCR: {}", ret);
            Err(ret)
        }
    }
    /// Cause conditional gating of a policy based on PCR.
    ///
    /// The TPM will use the hash algorithm of the policy_session
    /// to calculate a digest from the values of the pcr slots
    /// specified in the pcr_selections.
    /// This is then compared to pcr_policy_digest if they match then
    /// the policyDigest of the policy session is extended.
    ///
    /// # Constraints
    /// * `pcr_policy_digest` must be at most 64 elements long
    ///
    /// # Errors
    /// * if the pcr policy digest provided is too long, a `WrongParamSize` wrapper error will be returned
    ///
    /// See:
    /// "Trusted Platform Module Library",
    /// "Part 3: Commands"
    /// "Family “2.0”
    /// Level 00 Revision 01.59
    /// Section: 23.7 TPM2_PolicyPCR
    pub fn policy_pcr(
        &mut self,
        policy_session: ESYS_TR,
        pcr_policy_digest: &[u8],
        pcr_selections: PcrSelections,
    ) -> Result<()> {
        let pcr_digest = wrap_buffer!(pcr_policy_digest, TPM2B_DIGEST, 64);
        let ret = unsafe {
            Esys_PolicyPCR(
                self.mut_context(),
                policy_session,
                self.sessions.0,
                self.sessions.1,
                self.sessions.2,
                &pcr_digest,
                &pcr_selections.into(),
            )
        };
        let ret = Error::from_tss_rc(ret);
        if ret.is_success() {
            Ok(())
        } else {
            Err(ret)
        }
    }

    // TODO: Should we really keep `num_bytes` as `u16`?
    /// Get a number of random bytes from the TPM and return them.
    ///
    /// # Errors
    /// * if converting `num_bytes` to `u16` fails, a `WrongParamSize` will be returned
    pub fn get_random(&mut self, num_bytes: usize) -> Result<Vec<u8>> {
        let mut buffer = null_mut();
        let ret = unsafe {
            Esys_GetRandom(
                self.mut_context(),
                self.sessions.0,
                self.sessions.1,
                self.sessions.2,
                num_bytes
                    .try_into()
                    .or_else(|_| Err(Error::local_error(ErrorKind::WrongParamSize)))?,
                &mut buffer,
            )
        };

        let ret = Error::from_tss_rc(ret);
        if ret.is_success() {
            let buffer = unsafe { MBox::from_raw(buffer) };
            let mut random = buffer.buffer.to_vec();
            random.truncate(buffer.size.try_into().unwrap()); // should not panic given the TryInto above
            Ok(random)
        } else {
            error!("Error in flushing context: {}.", ret);
            Err(ret)
        }
    }

    /// Test if the given parameters are supported by the TPM.
    ///
    /// # Errors
    /// * if any of the public parameters is not compatible with the TPM,
    /// an `Err` containing the specific unmarshalling error will be returned.
    pub fn test_parms(&mut self, parms: PublicParmsUnion) -> Result<()> {
        let public_parms = TPMT_PUBLIC_PARMS {
            type_: parms.object_type(),
            parameters: parms.into(),
        };
        let ret = unsafe {
            Esys_TestParms(
                self.mut_context(),
                self.sessions.0,
                self.sessions.1,
                self.sessions.2,
                &public_parms,
            )
        };

        let ret = Error::from_tss_rc(ret);
        if ret.is_success() {
            Ok(())
        } else {
            error!("Error while testing parameters: {}.", ret);
            Err(ret)
        }
    }

    /// Function for invoking TPM2_Hash command.
    ///
    pub fn hash(
        &mut self,
        data: &[u8],
        hashing_algorithm: HashingAlgorithm,
        hierarchy: Hierarchy,
    ) -> Result<(Vec<u8>, HashcheckTicket)> {
        let data = wrap_buffer!(data, TPM2B_MAX_BUFFER, 1024);
        let mut out_hash_ptr = null_mut();
        let mut validation_ptr = null_mut();
        let ret = unsafe {
            Esys_Hash(
                self.mut_context(),
                self.sessions.0,
                self.sessions.1,
                self.sessions.2,
                &data,
                hashing_algorithm.into(),
                hierarchy.rh(),
                &mut out_hash_ptr,
                &mut validation_ptr,
            )
        };
        let ret = Error::from_tss_rc(ret);
        if ret.is_success() {
            let out_hash = unsafe { MBox::<TPM2B_DIGEST>::from_raw(out_hash_ptr) };
            let validation = unsafe { MBox::<TPMT_TK_HASHCHECK>::from_raw(validation_ptr) };
            Ok((
                out_hash.buffer[..out_hash.size as usize].to_vec(),
                HashcheckTicket::try_from(*validation)?,
            ))
        } else {
            error!("Error failed to peform hash operation: {}.", ret);
            Err(ret)
        }
    }

    /// Function for retriving the current policy digest for
    /// the session.
    pub fn policy_get_digest(&mut self, policy_session: ESYS_TR) -> Result<Vec<u8>> {
        let mut policy_digest_ptr = null_mut();
        let ret = unsafe {
            Esys_PolicyGetDigest(
                self.mut_context(),
                policy_session,
                self.sessions.0,
                self.sessions.1,
                self.sessions.2,
                &mut policy_digest_ptr,
            )
        };
        let ret = Error::from_tss_rc(ret);
        if ret.is_success() {
            let policy_digest = unsafe { MBox::<TPM2B_DIGEST>::from_raw(policy_digest_ptr) };
            Ok(policy_digest.buffer[..policy_digest.size as usize].to_vec())
        } else {
            error!(
                "Error failed to peform policy get digest operation: {}.",
                ret
            );
            Err(ret)
        }
    }

    ///////////////////////////////////////////////////////////////////////////
    /// TPM Resource Section
    ///////////////////////////////////////////////////////////////////////////

    /// Set the authentication value for a given object handle in the ESYS context.
    ///
    /// # Constraints
    /// * `auth_value` must be at most 64 elements long
    ///
    /// # Errors
    /// * if `auth_value` is larger than the limit, a `WrongParamSize` wrapper error is returned
    pub fn tr_set_auth(&mut self, handle: ESYS_TR, auth_value: &[u8]) -> Result<()> {
        let auth = wrap_buffer!(auth_value, TPM2B_AUTH, 64);
        let ret = unsafe { Esys_TR_SetAuth(self.mut_context(), handle, &auth) };
        let ret = Error::from_tss_rc(ret);
        if ret.is_success() {
            Ok(())
        } else {
            Err(ret)
        }
    }

    /// Retrieve the name of an object from the object handle
    pub fn tr_get_name(&mut self, handle: ESYS_TR) -> Result<TPM2B_NAME> {
        let mut name = null_mut();
        let ret = unsafe { Esys_TR_GetName(self.mut_context(), handle, &mut name) };
        let ret = Error::from_tss_rc(ret);
        if ret.is_success() {
            let name = unsafe { MBox::<TPM2B_NAME>::from_raw(name) };
            Ok(*name)
        } else {
            error!("Error in getting name: {}.", ret);
            Err(ret)
        }
    }

    /// Set the given attributes on a given session.
    pub fn tr_sess_set_attributes(
        &mut self,
        handle: ESYS_TR,
        attributes: TpmaSession,
    ) -> Result<()> {
        let ret = unsafe {
            Esys_TRSess_SetAttributes(
                self.mut_context(),
                handle,
                attributes.flags(),
                attributes.mask(),
            )
        };
        let ret = Error::from_tss_rc(ret);
        if ret.is_success() {
            Ok(())
        } else {
            Err(ret)
        }
    }

    /// Get session attribute flags.
    pub fn tr_sess_get_attributes(&mut self, handle: ESYS_TR) -> Result<TpmaSession> {
        let mut flags: TPMA_SESSION = 0;
        let ret = unsafe { Esys_TRSess_GetAttributes(self.mut_context(), handle, &mut flags) };
        let ret = Error::from_tss_rc(ret);
        if ret.is_success() {
            Ok(TpmaSessionBuilder::new().with_flag(flags).build())
        } else {
            Err(ret)
        }
    }

    ///////////////////////////////////////////////////////////////////////////
    /// Private Methods Section
    ///////////////////////////////////////////////////////////////////////////

    /// Returns a mutable reference to the native ESYS context handle.
    fn mut_context(&mut self) -> *mut ESYS_CONTEXT {
        self.esys_context.as_mut().unwrap().as_mut_ptr() // will only fail if called from Drop after .take()
    }
}

impl Drop for Context {
    fn drop(&mut self) {
        info!("Closing context.");

        // Flush the open handles.
        self.open_handles.clone().iter().for_each(|handle| {
            info!("Flushing handle {}", *handle);
            if let Err(e) = self.flush_context(*handle) {
                error!("Error when dropping the context: {}.", e);
            }
        });

        let esys_context = self.esys_context.take().unwrap(); // should not fail based on how the context is initialised/used
        let tcti_context = self.tcti_context.take().unwrap(); // should not fail based on how the context is initialised/used

        // Close the TCTI context.
        unsafe { Tss2_TctiLdr_Finalize(&mut tcti_context.into_raw()) };

        // Close the context.
        unsafe { Esys_Finalize(&mut esys_context.into_raw()) };
        info!("Context closed.");
    }
}
