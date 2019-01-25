use elements::Proof;
use bitcoin::Script;

use crate::util::get_script_asm;

#[derive(Serialize, Deserialize)]
pub struct BlockProofValue {
    challenge: Script,
    challenge_asm: String,
    solution: Script,
    solution_asm: String,
}

impl From<&Proof> for BlockProofValue {
    fn from(proof: &Proof) -> Self {
        BlockProofValue {
            challenge_asm: get_script_asm(&proof.challenge),
            challenge: proof.challenge.clone(),
            solution_asm: get_script_asm(&proof.solution),
            solution: proof.solution.clone(),
        }
    }
}
