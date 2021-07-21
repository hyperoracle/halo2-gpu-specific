//! Developer tools for investigating the cost of a circuit.

use std::{
    collections::{HashMap, HashSet},
    iter,
    marker::PhantomData,
};

use ff::PrimeField;
use group::prime::PrimeGroup;

use crate::{
    plonk::{Any, Circuit, Column, ConstraintSystem},
    poly::Rotation,
};

/// Measures a circuit to determine its costs, and explain what contributes to them.
#[derive(Debug)]
pub struct CircuitCost<G: PrimeGroup, ConcreteCircuit: Circuit<G::Scalar>> {
    /// Power-of-2 bound on the number of rows in the circuit.
    k: usize,
    /// Maximum degree of the circuit.
    max_deg: usize,
    /// Number of advice columns.
    advice_columns: usize,
    /// Number of direct queries for each column type.
    instance_queries: usize,
    advice_queries: usize,
    fixed_queries: usize,
    /// Number of lookup arguments.
    lookups: usize,
    /// Number of columns in the global permutation.
    permutation_cols: usize,
    /// Number of distinct sets of points in the multiopening argument.
    point_sets: usize,

    _marker: PhantomData<(G, ConcreteCircuit)>,
}

impl<G: PrimeGroup, ConcreteCircuit: Circuit<G::Scalar>> CircuitCost<G, ConcreteCircuit> {
    /// Measures a circuit with parameter constant `k`.
    ///
    /// Panics if `k` is not large enough for the circuit.
    pub fn measure(k: usize) -> Self {
        // Collect the layout details.
        let mut cs = ConstraintSystem::default();
        let _ = ConcreteCircuit::configure(&mut cs);
        assert!((1 << k) >= cs.minimum_rows());

        // Figure out how many point sets we have due to queried cells.
        let mut column_queries: HashMap<Column<Any>, HashSet<i32>> = HashMap::new();
        for (c, r) in iter::empty()
            .chain(
                cs.advice_queries
                    .iter()
                    .map(|(c, r)| (Column::<Any>::from(*c), *r)),
            )
            .chain(cs.instance_queries.iter().map(|(c, r)| ((*c).into(), *r)))
            .chain(cs.fixed_queries.iter().map(|(c, r)| ((*c).into(), *r)))
            .chain(
                cs.permutation
                    .get_columns()
                    .into_iter()
                    .map(|c| (c, Rotation::cur())),
            )
        {
            column_queries.entry(c).or_default().insert(r.0);
        }
        let mut point_sets: HashSet<Vec<i32>> = HashSet::new();
        for (_, r) in column_queries {
            // Sort the query sets so we merge duplicates.
            let mut query_set: Vec<_> = r.into_iter().collect();
            query_set.sort_unstable();
            point_sets.insert(query_set);
        }

        // Include lookup polynomials in point sets:
        point_sets.insert(vec![0, 1]); // product_poly
        point_sets.insert(vec![-1, 0]); // permuted_input_poly
        point_sets.insert(vec![0]); // permuted_table_poly

        // Include permutation polynomials in point sets.
        point_sets.insert(vec![0, 1]); // permutation_product_poly
        let max_deg = cs.degree();
        let permutation_cols = cs.permutation.get_columns().len();
        if permutation_cols > max_deg - 2 {
            // permutation_product_poly for chaining chunks.
            point_sets.insert(vec![-((cs.blinding_factors() + 1) as i32), 0, 1]);
        }

        CircuitCost {
            k,
            max_deg,
            advice_columns: cs.num_advice_columns,
            instance_queries: cs.instance_queries.len(),
            advice_queries: cs.advice_queries.len(),
            fixed_queries: cs.fixed_queries.len(),
            lookups: cs.lookups.len(),
            permutation_cols,
            point_sets: point_sets.len(),
            _marker: PhantomData::default(),
        }
    }

    fn permutation_chunks(&self) -> usize {
        let chunk_size = self.max_deg - 2;
        (self.permutation_cols + chunk_size - 1) / chunk_size
    }

    /// Returns the proof size for the given number of instances of this circuit.
    pub fn proof_size(&self, instances: usize) -> ProofSize<G> {
        let chunks = self.permutation_chunks();

        ProofSize {
            // Cells:
            // - 1 commitment per advice column per instance
            // - 1 eval per instance column query per instance
            // - 1 eval per advice column query per instance
            // - 1 eval per fixed column query
            instance: ProofContribution::new(0, self.instance_queries * instances),
            advice: ProofContribution::new(
                self.advice_columns * instances,
                self.advice_queries * instances,
            ),
            fixed: ProofContribution::new(0, self.fixed_queries),

            // Lookup arguments:
            // - 3 commitments per lookup argument per instance
            // - 5 evals per lookup argument per instance
            lookups: ProofContribution::new(
                3 * self.lookups * instances,
                5 * self.lookups * instances,
            ),

            // Global permutation argument:
            // - chunks commitments per instance
            // - 2*chunks + (chunks - 1) evals per instance
            // - 1 eval per column
            equality: ProofContribution::new(
                chunks * instances,
                (3 * chunks - 1) * instances + self.permutation_cols,
            ),

            // Vanishing argument:
            // - 1 + (max_deg - 1) commitments
            // - 1 random_poly eval
            vanishing: ProofContribution::new(self.max_deg, 1),

            // Multiopening argument:
            // - f_commitment
            // - 1 eval per set of points in multiopen argument
            multiopen: ProofContribution::new(1, self.point_sets),

            // Polycommit:
            // - s_poly commitment
            // - inner product argument (2 * k round commitments)
            // - a
            // - xi
            polycomm: ProofContribution::new(1 + 2 * self.k, 2),

            _marker: PhantomData::default(),
        }
    }
}

/// (commitments, evaluations)
#[derive(Debug)]
struct ProofContribution {
    commitments: usize,
    evaluations: usize,
}

impl ProofContribution {
    fn new(commitments: usize, evaluations: usize) -> Self {
        ProofContribution {
            commitments,
            evaluations,
        }
    }

    fn len(&self, point: usize, scalar: usize) -> usize {
        self.commitments * point + self.evaluations * scalar
    }
}

/// The size of a Halo 2 proof, broken down into its contributing factors.
#[derive(Debug)]
pub struct ProofSize<G: PrimeGroup> {
    instance: ProofContribution,
    advice: ProofContribution,
    fixed: ProofContribution,
    lookups: ProofContribution,
    equality: ProofContribution,
    vanishing: ProofContribution,
    multiopen: ProofContribution,
    polycomm: ProofContribution,
    _marker: PhantomData<G>,
}

impl<G: PrimeGroup> From<ProofSize<G>> for usize {
    fn from(proof: ProofSize<G>) -> Self {
        let point = G::Repr::default().as_ref().len();
        let scalar = <G::Scalar as PrimeField>::Repr::default().as_ref().len();

        proof.instance.len(point, scalar)
            + proof.advice.len(point, scalar)
            + proof.fixed.len(point, scalar)
            + proof.lookups.len(point, scalar)
            + proof.equality.len(point, scalar)
            + proof.vanishing.len(point, scalar)
            + proof.multiopen.len(point, scalar)
            + proof.polycomm.len(point, scalar)
    }
}