#![cfg_attr(not(feature = "std"), no_std)]

//! Schnorr protocol to prove knowledge of 1 or more discrete logs in zero knowledge. Refer <https://crypto.stanford.edu/cs355/19sp/lec5.pdf>

use crate::error::SchnorrError;
use ark_ec::msm::VariableBaseMSM;
use ark_ec::{AffineCurve, ProjectiveCurve};
use ark_ff::PrimeField;
use ark_serialize::{CanonicalDeserialize, CanonicalSerialize, SerializationError};
use ark_std::{
    fmt::Debug,
    io::{Read, Write},
    vec::Vec,
};
use digest::Digest;

use dock_crypto_utils::hashing_utils::field_elem_from_try_and_incr;

#[cfg(feature = "use-serde")]
use dock_crypto_utils::serde_utils::*;
#[cfg(feature = "use-serde")]
use serde::{Deserialize, Serialize};
#[cfg(feature = "use-serde")]
use serde_with::serde_as;

#[cfg(feature = "parallel")]
use rayon::prelude::*;

pub mod error;

/// Return `par_iter` or `iter` depending on whether feature `parallel` is enabled
macro_rules! iter {
    ($val:expr) => {{
        #[cfg(feature = "parallel")]
        let it = $val.par_iter();
        #[cfg(not(feature = "parallel"))]
        let it = $val.iter();
        it
    }};
}

/// Trait implemented by Schnorr-based protocols for returning their contribution to the overall challenge.
pub trait SchnorrChallengeContributor {
    fn challenge_contribution<W: Write>(&self, writer: W) -> Result<(), SchnorrError>;
}

/// Commitment to randomness during step 1 of the Schnorr protocol to prove knowledge of 1 or more discrete logs
#[cfg_attr(feature = "use-serde", serde_as)]
#[cfg_attr(feature = "use-serde", derive(Serialize, Deserialize))]
#[derive(Clone, Debug, PartialEq, Eq, CanonicalSerialize, CanonicalDeserialize)]
pub struct SchnorrCommitment<G: AffineCurve> {
    /// Randomness. 1 per discrete log
    #[serde_as(as = "Vec<ScalarFieldBytes>")]
    pub blindings: Vec<G::ScalarField>,
    /// The commitment to all the randomnesses
    #[serde_as(as = "AffineGroupBytes")]
    pub t: G,
}

impl<G> SchnorrCommitment<G>
where
    G: AffineCurve,
{
    /// Create commitment as `bases[0] * blindings[0] + bases[1] * blindings[1] + ... bases[i] * blindings[i]`
    /// for step-1 of the protocol. Extra `bases` or `blindings` are ignored.
    pub fn new(bases: &[G], blindings: Vec<G::ScalarField>) -> Self {
        let scalars = iter!(blindings).map(|b| b.into_repr()).collect::<Vec<_>>();
        let t = VariableBaseMSM::multi_scalar_mul(bases, &scalars).into_affine();
        Self { blindings, t }
    }

    /// Create responses for each witness (discrete log) as `response[i] = self.blindings[i] + (witnesses[i] * challenge)`
    pub fn response(
        &self,
        witnesses: &[G::ScalarField],
        challenge: &G::ScalarField,
    ) -> Result<SchnorrResponse<G>, SchnorrError> {
        if self.blindings.len() != witnesses.len() {
            return Err(SchnorrError::ExpectedSameSizeSequences(
                self.blindings.len(),
                witnesses.len(),
            ));
        }
        let responses = iter!(self.blindings)
            .zip(iter!(witnesses))
            .map(|(b, w)| *b + (*w * *challenge))
            .collect::<Vec<_>>();
        Ok(SchnorrResponse(responses))
    }
}

impl<G> SchnorrChallengeContributor for SchnorrCommitment<G>
where
    G: AffineCurve,
{
    /// The commitment's contribution to the overall challenge of the protocol. Note that
    /// it does not include the bases or the commitment (`g_`  and `y` in `{g_i}^x_i = y`) and
    /// they must be part of the challenge.
    fn challenge_contribution<W: Write>(&self, writer: W) -> Result<(), SchnorrError> {
        self.t.serialize_unchecked(writer).map_err(|e| e.into())
    }
}

/// Response during step 3 of the Schnorr protocol to prove knowledge of 1 or more discrete logs
#[cfg_attr(feature = "use-serde", serde_as)]
#[cfg_attr(feature = "use-serde", derive(Serialize, Deserialize))]
#[derive(Clone, Debug, PartialEq, Eq, CanonicalSerialize, CanonicalDeserialize)]
pub struct SchnorrResponse<G: AffineCurve>(
    #[serde_as(as = "Vec<ScalarFieldBytes>")] pub Vec<G::ScalarField>,
);

impl<G> SchnorrResponse<G>
where
    G: AffineCurve,
{
    /// Check if response is valid and thus validity of Schnorr proof
    /// bases[0]*responses[0] + bases[0]*responses[0] + ... bases[i]*responses[i] - y*challenge == t
    pub fn is_valid(
        &self,
        bases: &[G],
        y: &G,
        t: &G,
        challenge: &G::ScalarField,
    ) -> Result<(), SchnorrError> {
        if self.0.len() != bases.len() {
            return Err(SchnorrError::ExpectedSameSizeSequences(
                self.0.len(),
                bases.len(),
            ));
        }
        let mut bases = bases.to_vec();
        bases.push(*y);
        let mut scalars = iter!(self.0).map(|r| r.into_repr()).collect::<Vec<_>>();
        scalars.push((-*challenge).into_repr());
        if VariableBaseMSM::multi_scalar_mul(&bases, scalars.as_slice()).into_affine() == *t {
            Ok(())
        } else {
            return Err(SchnorrError::InvalidResponse);
        }
    }

    /// Get response for the specified discrete log
    pub fn get_response(&self, idx: usize) -> Result<&G::ScalarField, SchnorrError> {
        if idx >= self.0.len() {
            return Err(SchnorrError::IndexOutOfBounds(idx, self.0.len()));
        }
        Ok(&self.0[idx])
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }
    // TODO: Add function for challenge contribution (bytes that are hashed)
}

// Proof of knowledge of a single discrete log

#[macro_export]
macro_rules! impl_proof_of_knowledge_of_discrete_log {
    ($protocol_name:ident, $proof_name: ident) => {
        /// Proof of knowledge protocol for discrete log
        #[derive(Clone, PartialEq, Eq, Debug, CanonicalSerialize, CanonicalDeserialize)]
        pub struct $protocol_name<G: AffineCurve> {
            pub t: G,
            blinding: G::ScalarField,
            witness: G::ScalarField,
        }

        #[derive(Clone, PartialEq, Eq, Debug, CanonicalSerialize, CanonicalDeserialize)]
        pub struct $proof_name<G: AffineCurve> {
            pub t: G,
            pub response: G::ScalarField,
        }

        impl<G> $protocol_name<G>
        where
            G: AffineCurve,
        {
            pub fn init(witness: G::ScalarField, blinding: G::ScalarField, base: &G) -> Self {
                let t = base.mul(blinding.into_repr()).into_affine();
                Self {
                    t,
                    blinding,
                    witness,
                }
            }

            pub fn challenge_contribution<W: Write>(&self, writer: W) -> Result<(), SchnorrError> {
                self.t.serialize_unchecked(writer).map_err(|e| e.into())
            }

            pub fn gen_proof(self, challenge: &G::ScalarField) -> $proof_name<G> {
                let response = self.blinding + (self.witness * *challenge);
                $proof_name {
                    t: self.t,
                    response,
                }
            }
        }

        impl<G> $proof_name<G>
        where
            G: AffineCurve,
        {
            pub fn challenge_contribution<W: Write>(&self, writer: W) -> Result<(), SchnorrError> {
                self.t.serialize_unchecked(writer).map_err(|e| e.into())
            }

            /// base*response - y*challenge == t
            pub fn verify(&self, y: &G, base: &G, challenge: &G::ScalarField) -> bool {
                let mut expected = base.mul(self.response.into_repr());
                expected -= y.mul(challenge.into_repr());
                expected.into_affine() == self.t
            }
        }
    };
}

/// Uses try-and-increment. Vulnerable to side channel attacks.
pub fn compute_random_oracle_challenge<F: PrimeField, D: Digest>(challenge_bytes: &[u8]) -> F {
    field_elem_from_try_and_incr::<F, D>(challenge_bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ark_bls12_381::Bls12_381;
    use ark_ec::PairingEngine;
    use ark_std::{
        rand::{rngs::StdRng, SeedableRng},
        UniformRand,
    };

    type Fr = <Bls12_381 as PairingEngine>::Fr;

    #[macro_export]
    macro_rules! test_serialization {
        ($obj_type:ident, $obj: ident) => {
            let mut serz = vec![];
            $obj.serialize(&mut serz).unwrap();
            println!("serialize size {}", serz.len());
            assert_eq!($obj_type::deserialize(&serz[..]).unwrap(), $obj);

            let mut serz = vec![];
            $obj.serialize_unchecked(&mut serz).unwrap();
            println!("serialize_unchecked size {}", serz.len());
            assert_eq!($obj_type::deserialize_unchecked(&serz[..]).unwrap(), $obj);

            let mut serz = vec![];
            $obj.serialize_uncompressed(&mut serz).unwrap();
            println!("serialize_uncompressed size {}", serz.len());
            assert_eq!(
                $obj_type::deserialize_uncompressed(&serz[..]).unwrap(),
                $obj
            );
        };
    }

    macro_rules! test_schnorr_in_group {
        ( $group_element:ident ) => {
            let mut rng = StdRng::seed_from_u64(0u64);
            let count = 10;
            let bases = (0..count)
                .into_iter()
                .map(|_| <Bls12_381 as PairingEngine>::$group_element::rand(&mut rng).into_affine())
                .collect::<Vec<_>>();
            let witnesses = (0..count)
                .into_iter()
                .map(|_| Fr::rand(&mut rng))
                .collect::<Vec<_>>();

            let y = VariableBaseMSM::multi_scalar_mul(
                &bases,
                &witnesses.iter().map(|w| w.into_repr()).collect::<Vec<_>>(),
            )
            .into_affine();

            let blindings = (0..count)
                .into_iter()
                .map(|_| Fr::rand(&mut rng))
                .collect::<Vec<_>>();

            let comm = SchnorrCommitment::new(&bases, blindings);
            test_serialization!(SchnorrCommitment, comm);

            let challenge = Fr::rand(&mut rng);

            let resp = comm.response(&witnesses, &challenge).unwrap();

            resp.is_valid(&bases, &y, &comm.t, &challenge).unwrap();

            let mut serz = vec![];
            resp.serialize(&mut serz).unwrap();
            test_serialization!(SchnorrResponse, resp);
        };
    }

    #[test]
    fn schnorr_vector() {
        test_schnorr_in_group!(G1Projective);
        test_schnorr_in_group!(G2Projective);
    }

    #[test]
    fn schnorr_single() {
        let mut rng = StdRng::seed_from_u64(0u64);

        macro_rules! check {
            ($protocol_name:ident, $proof_name: ident, $group_affine:ident, $group_projective:ident) => {
                impl_proof_of_knowledge_of_discrete_log!($protocol_name, $proof_name);
                let base =
                    <Bls12_381 as PairingEngine>::$group_projective::rand(&mut rng).into_affine();
                let witness = Fr::rand(&mut rng);
                let y = base.mul(witness.into_repr()).into_affine();
                let blinding = Fr::rand(&mut rng);
                let protocol = $protocol_name::<<Bls12_381 as PairingEngine>::$group_affine>::init(
                    witness, blinding, &base,
                );
                let mut chal_contrib_prover = vec![];
                protocol
                    .challenge_contribution(&mut chal_contrib_prover)
                    .unwrap();

                let mut serz = vec![];
                protocol.serialize(&mut serz).unwrap();

                test_serialization!($protocol_name, protocol);

                let challenge = Fr::rand(&mut rng);
                let proof = protocol.gen_proof(&challenge);

                let mut chal_contrib_verifier = vec![];
                proof
                    .challenge_contribution(&mut chal_contrib_verifier)
                    .unwrap();

                assert!(proof.verify(&y, &base, &challenge));
                assert_eq!(chal_contrib_prover, chal_contrib_verifier);

                test_serialization!($proof_name, proof);
            };
        }

        check!(Protocol1, Proof1, G1Affine, G1Projective);
        check!(Protocol2, Proof2, G2Affine, G2Projective);
    }
}
