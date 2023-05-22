use crate::threshold::cointoss::Commitments;
use ark_ec::{pairing::Pairing, AffineRepr, CurveGroup};
use ark_ff::{Field, PrimeField, Zero};

use ark_serialize::{CanonicalDeserialize, CanonicalSerialize};
use ark_std::{
    collections::{BTreeMap, BTreeSet},
    rand::RngCore,
    vec::Vec,
};
use digest::DynDigest;
use oblivious_transfer_protocols::ParticipantId;

use crate::{
    error::BBSPlusError,
    setup::{MultiMessageSignatureParams, SignatureParamsG1},
    signature::SignatureG1,
    threshold::randomness_generation_phase::Phase1,
};

use super::{multiplication_phase::Phase2Output, utils::compute_R_and_u};

/// The length of vectors `r`, `e`, `s`, `masked_signing_key_shares`, `masked_rs` should
/// be `batch_size` each item of the vector corresponds to 1 signature
#[derive(Clone, Debug, PartialEq, CanonicalSerialize, CanonicalDeserialize)]
pub struct Phase1Output<F: PrimeField> {
    pub id: ParticipantId,
    pub batch_size: usize,
    pub r: Vec<F>,
    pub e: Vec<F>,
    pub s: Vec<F>,
    /// Additive shares of the signing key masked by a random `alpha`
    pub masked_signing_key_shares: Vec<F>,
    /// Additive shares of `r` masked by a random `beta`
    pub masked_rs: Vec<F>,
    pub others: Vec<ParticipantId>,
}

/// A share of the BBS+ signature created by one signer. A client will aggregate many such shares to
/// create the final signature.
pub struct BBSPlusSignatureShare<E: Pairing> {
    pub id: ParticipantId,
    pub e: E::ScalarField,
    pub s: E::ScalarField,
    pub u: E::ScalarField,
    pub R: E::G1Affine,
}

impl<F: PrimeField, const SALT_SIZE: usize> Phase1<F, SALT_SIZE> {
    pub fn init_for_bbs_plus<R: RngCore>(
        rng: &mut R,
        batch_size: usize,
        id: ParticipantId,
        others: BTreeSet<ParticipantId>,
        protocol_id: Vec<u8>,
    ) -> (Self, Commitments, BTreeMap<ParticipantId, Commitments>) {
        let r = (0..batch_size).map(|_| F::rand(rng)).collect();
        // 2 because 2 random values `e` and `s` need to be generated per signature
        let (commitment_protocol, comm) =
            super::cointoss::Party::commit(rng, id, 2 * batch_size, protocol_id.clone());
        // Each signature will have its own zero-sharing of `alpha` and `beta`
        let (zero_sharing_protocol, comm_zero_share) =
            super::zero_sharing::Party::init(rng, id, 2 * batch_size, others, protocol_id);
        (
            Self {
                id,
                batch_size,
                r,
                commitment_protocol,
                zero_sharing_protocol,
            },
            comm,
            comm_zero_share,
        )
    }

    pub fn finish_for_bbs_plus<D: Default + DynDigest + Clone>(
        self,
        signing_key: &F,
    ) -> Result<Phase1Output<F>, BBSPlusError> {
        // TODO: Ensure every one has participated in both protocols
        let id = self.id;
        let batch_size = self.batch_size;
        let r = self.r.clone();
        let (others, mut randomness, masked_signing_key_shares, masked_rs) =
            self.compute_joint_randomness_and_masked_arguments_to_multiply::<D>(signing_key)?;
        debug_assert_eq!(randomness.len(), 2 * batch_size);
        let e = randomness.drain(0..batch_size).collect();
        let s = randomness;
        Ok(Phase1Output {
            id,
            batch_size,
            r,
            e,
            s,
            masked_signing_key_shares,
            masked_rs,
            others,
        })
    }
}

impl<E: Pairing> BBSPlusSignatureShare<E> {
    /// `index_in_output` is the index of this signature in the Phase1 and Phase2 outputs
    pub fn new(
        messages: &[E::ScalarField],
        index_in_output: usize,
        phase1: &Phase1Output<E::ScalarField>,
        phase2: &Phase2Output<E::ScalarField>,
        sig_params: &SignatureParamsG1<E>,
    ) -> Result<Self, BBSPlusError> {
        if messages.is_empty() {
            return Err(BBSPlusError::NoMessageToSign);
        }
        if messages.len() != sig_params.supported_message_count() {
            return Err(BBSPlusError::MessageCountIncompatibleWithSigParams(
                messages.len(),
                sig_params.supported_message_count(),
            ));
        }
        // Create map of msg index (0-based) -> message
        let msg_map: BTreeMap<usize, &E::ScalarField> =
            messages.iter().enumerate().map(|(i, e)| (i, e)).collect();
        Self::new_with_committed_messages(
            &E::G1Affine::zero(),
            msg_map,
            index_in_output,
            phase1,
            phase2,
            sig_params,
        )
    }

    /// `index_in_output` is the index of this signature in the Phase1 and Phase2 outputs
    pub fn new_with_committed_messages(
        commitment: &E::G1Affine,
        uncommitted_messages: BTreeMap<usize, &E::ScalarField>,
        index_in_output: usize,
        phase1: &Phase1Output<E::ScalarField>,
        phase2: &Phase2Output<E::ScalarField>,
        sig_params: &SignatureParamsG1<E>,
    ) -> Result<Self, BBSPlusError> {
        let b = sig_params.b(uncommitted_messages, &phase1.s[index_in_output])?;
        let commitment_plus_b = b + commitment;
        let (R, u) = compute_R_and_u(
            commitment_plus_b,
            &phase1.r[index_in_output],
            &phase1.e[index_in_output],
            &phase1.masked_rs[index_in_output],
            &phase1.masked_signing_key_shares[index_in_output],
            index_in_output,
            phase2,
        );
        Ok(Self {
            id: phase1.id,
            e: phase1.e[index_in_output],
            s: phase1.s[index_in_output],
            u,
            R,
        })
    }

    pub fn aggregate(sig_shares: Vec<Self>) -> Result<SignatureG1<E>, BBSPlusError> {
        // TODO: Ensure correct threshold. Share should contain threshold and share id
        let mut sum_R = E::G1::zero();
        let mut sum_u = E::ScalarField::zero();
        let mut expected_e = E::ScalarField::zero();
        let mut expected_s = E::ScalarField::zero();
        for (i, share) in sig_shares.into_iter().enumerate() {
            if i == 0 {
                expected_e = share.e;
                expected_s = share.s;
            } else {
                if expected_e != share.e {
                    return Err(BBSPlusError::IncorrectEByParticipant(share.id));
                }
                if expected_s != share.s {
                    return Err(BBSPlusError::IncorrectSByParticipant(share.id));
                }
            }
            sum_u += share.u;
            sum_R += share.R;
        }
        let A = sum_R * sum_u.inverse().unwrap();
        Ok(SignatureG1 {
            A: A.into_affine(),
            e: expected_e,
            s: expected_s,
        })
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use ark_bls12_381::Bls12_381;
    use ark_ec::pairing::Pairing;
    use ark_ff::Zero;
    use ark_poly::{univariate::DensePolynomial, DenseUVPolynomial, Polynomial};
    use std::time::{Duration, Instant};

    use crate::{
        setup::{PublicKeyG2, SecretKey},
        threshold::{
            base_ot_phase::tests::do_base_ot_for_threshold_sig, multiplication_phase::Phase2,
        },
    };
    use oblivious_transfer_protocols::ot_based_multiplication::{
        dkls18_mul_2p::MultiplicationOTEParams, dkls19_batch_mul_2p::GadgetVector,
    };

    use ark_std::{
        cfg_into_iter,
        rand::{rngs::StdRng, SeedableRng},
        UniformRand,
    };
    use blake2::Blake2b512;

    use rayon::prelude::*;

    type Fr = <Bls12_381 as Pairing>::ScalarField;

    // TODO: Remove and use from other crate
    pub fn deal_random_secret<R: RngCore, F: PrimeField>(
        rng: &mut R,
        threshold: ParticipantId,
        total: ParticipantId,
    ) -> (F, Vec<F>, DensePolynomial<F>) {
        let secret = F::rand(rng);
        let (shares, poly) = deal_secret(rng, secret.clone(), threshold, total);
        (secret, shares, poly)
    }
    pub fn deal_secret<R: RngCore, F: PrimeField>(
        rng: &mut R,
        secret: F,
        threshold: ParticipantId,
        total: ParticipantId,
    ) -> (Vec<F>, DensePolynomial<F>) {
        let mut coeffs = Vec::with_capacity(threshold as usize);
        coeffs.append(&mut (0..threshold - 1).map(|_| F::rand(rng)).collect());
        coeffs.insert(0, secret);
        let poly = DensePolynomial::from_coefficients_vec(coeffs);
        let shares = cfg_into_iter!((1..=total))
            .map(|i| poly.evaluate(&F::from(i as u64)))
            .collect::<Vec<_>>();
        (shares, poly)
    }

    #[test]
    fn signing() {
        let mut rng = StdRng::seed_from_u64(0u64);
        const BASE_OT_KEY_SIZE: u16 = 128;
        const KAPPA: u16 = 256;
        const STATISTICAL_SECURITY_PARAMETER: u16 = 80;
        let ote_params = MultiplicationOTEParams::<KAPPA, STATISTICAL_SECURITY_PARAMETER> {};
        let gadget_vector = GadgetVector::<Fr, KAPPA, STATISTICAL_SECURITY_PARAMETER>::new::<
            Blake2b512,
        >(ote_params, b"test-gadget-vector");

        fn check(
            rng: &mut StdRng,
            ote_params: MultiplicationOTEParams<KAPPA, STATISTICAL_SECURITY_PARAMETER>,
            num_signers: u16,
            sig_batch_size: usize,
            message_count: usize,
            gadget_vector: &GadgetVector<Fr, KAPPA, STATISTICAL_SECURITY_PARAMETER>,
        ) {
            let protocol_id = b"test".to_vec();

            let all_party_set = (1..=num_signers).into_iter().collect::<BTreeSet<_>>();
            let (sk, sk_shares, _poly) = deal_random_secret::<_, Fr>(rng, num_signers, num_signers);
            let params = SignatureParamsG1::<Bls12_381>::generate_using_rng(rng, message_count);
            let public_key = PublicKeyG2::generate_using_secret_key(&SecretKey(sk), &params);

            println!(
                "For a batch size of {} BBS+ signatures on messages of size {} and {} signers",
                sig_batch_size, message_count, num_signers
            );

            let start = Instant::now();
            let base_ot_outputs = do_base_ot_for_threshold_sig::<BASE_OT_KEY_SIZE>(
                rng,
                ote_params.num_base_ot(),
                num_signers,
                all_party_set.clone(),
            );
            println!("Base OT phase took {:?}", start.elapsed());

            let mut round1s = vec![];
            let mut commitments = vec![];
            let mut commitments_zero_share = vec![];
            let mut round1outs = vec![];

            let start = Instant::now();
            for i in 1..=num_signers {
                let mut others = all_party_set.clone();
                others.remove(&i);
                let (round1, comm, comm_zero) = Phase1::<Fr, 256>::init_for_bbs_plus(
                    rng,
                    sig_batch_size,
                    i,
                    others,
                    protocol_id.clone(),
                );
                round1s.push(round1);
                commitments.push(comm);
                commitments_zero_share.push(comm_zero);
            }

            for i in 1..=num_signers {
                for j in 1..=num_signers {
                    if i != j {
                        round1s[i as usize - 1]
                            .receive_commitment(
                                j,
                                commitments[j as usize - 1].clone(),
                                commitments_zero_share[j as usize - 1]
                                    .get(&i)
                                    .unwrap()
                                    .clone(),
                            )
                            .unwrap();
                    }
                }
            }

            for i in 1..=num_signers {
                for j in 1..=num_signers {
                    if i != j {
                        let share = round1s[j as usize - 1].get_comm_shares_and_salts();
                        let zero_share = round1s[j as usize - 1]
                            .get_comm_shares_and_salts_for_zero_sharing_protocol_with_other(&i);
                        round1s[i as usize - 1]
                            .receive_shares(j, share, zero_share)
                            .unwrap();
                    }
                }
            }

            let mut expected_sk = Fr::zero();
            for (i, round1) in round1s.into_iter().enumerate() {
                let out = round1
                    .finish_for_bbs_plus::<Blake2b512>(&sk_shares[i])
                    .unwrap();
                expected_sk += out.masked_signing_key_shares.iter().sum::<Fr>();
                round1outs.push(out);
            }
            println!("Phase 1 took {:?}", start.elapsed());

            assert_eq!(expected_sk, sk * Fr::from(sig_batch_size as u64));
            for i in 1..num_signers {
                assert_eq!(round1outs[0].e, round1outs[i as usize].e);
                assert_eq!(round1outs[0].s, round1outs[i as usize].s);
            }

            let mut round2s = vec![];
            let mut all_u = vec![];

            let start = Instant::now();
            for i in 1..=num_signers {
                let mut others = all_party_set.clone();
                others.remove(&i);
                let (phase, U) = Phase2::init(
                    rng,
                    i,
                    round1outs[i as usize - 1].masked_signing_key_shares.clone(),
                    round1outs[i as usize - 1].masked_rs.clone(),
                    base_ot_outputs[i as usize - 1].clone(),
                    others,
                    ote_params,
                    &gadget_vector,
                )
                .unwrap();
                round2s.push(phase);
                all_u.push((i, U));
            }

            let mut all_tau = vec![];
            for (sender_id, U) in all_u {
                for (receiver_id, (U_i, rlc, gamma)) in U {
                    let (tau, r, gamma) = round2s[receiver_id as usize - 1]
                        .receive_u::<Blake2b512>(sender_id, U_i, rlc, gamma, &gadget_vector)
                        .unwrap();
                    all_tau.push((receiver_id, sender_id, (tau, r, gamma)));
                }
            }

            for (sender_id, receiver_id, (tau, r, gamma)) in all_tau {
                round2s[receiver_id as usize - 1]
                    .receive_tau::<Blake2b512>(sender_id, tau, r, gamma, &gadget_vector)
                    .unwrap();
            }

            let round2_outputs = round2s.into_iter().map(|p| p.finish()).collect::<Vec<_>>();
            println!("Phase 2 took {:?}", start.elapsed());

            for i in 1..=num_signers {
                for (j, z_A) in &round2_outputs[i as usize - 1].z_A {
                    let z_B = round2_outputs[*j as usize - 1].z_B.get(&i).unwrap();
                    for k in 0..sig_batch_size {
                        assert_eq!(
                            z_A.0[k] + z_B.0[k],
                            round1outs[i as usize - 1].masked_signing_key_shares[k]
                                * round1outs[*j as usize - 1].masked_rs[k]
                        );
                        assert_eq!(
                            z_A.1[k] + z_B.1[k],
                            round1outs[i as usize - 1].masked_rs[k]
                                * round1outs[*j as usize - 1].masked_signing_key_shares[k]
                        );
                    }
                }
            }

            let mut sig_shares_time = Duration::default();
            let mut sig_aggr_time = Duration::default();
            for k in 0..sig_batch_size {
                let messages = (0..message_count)
                    .into_iter()
                    .map(|_| Fr::rand(rng))
                    .collect::<Vec<_>>();

                let mut shares = vec![];
                let start = Instant::now();
                for i in 0..num_signers as usize {
                    let share = BBSPlusSignatureShare::new(
                        &messages,
                        k,
                        &round1outs[i],
                        &round2_outputs[i],
                        &params,
                    )
                    .unwrap();
                    shares.push(share);
                }
                sig_shares_time += start.elapsed();

                let start = Instant::now();
                let sig = BBSPlusSignatureShare::aggregate(shares).unwrap();
                sig_aggr_time += start.elapsed();
                sig.verify(&messages, public_key.clone(), params.clone())
                    .unwrap();
            }

            println!("Generating signature shares took {:?}", sig_shares_time);
            println!("Aggregating signature shares took {:?}", sig_aggr_time);
        }

        check(&mut rng, ote_params, 5, 10, 3, &gadget_vector);
        check(&mut rng, ote_params, 5, 20, 3, &gadget_vector);
        check(&mut rng, ote_params, 5, 30, 3, &gadget_vector);
        check(&mut rng, ote_params, 10, 10, 3, &gadget_vector);
        check(&mut rng, ote_params, 20, 10, 3, &gadget_vector);
    }
}