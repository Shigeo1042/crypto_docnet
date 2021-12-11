# PoC of verifiable encryption using SAVER

Prototype implementation of [SAVER](https://eprint.iacr.org/2019/1270). Implemented 
- using [Groth16](src/saver_groth16.rs) 
- as well as [LegoGroth16](src/saver_legogroth16.rs).

The encryption outputs a commitment in addition to the ciphertext. For an encryption of message `m`, the commitment `phi` is of the following form:

```
phi = m_1*Y_1 + m_2*Y_2 + ... + m_n*Y_n + r*P_2  
```

`m_i` are the bit decomposition of the original message `m` such that `m_1*{b^{n-1}} + m_2*{b^{n-2}} + .. + m_n` (big-endian) with `b` being the radix in which `m` is decomposed and `r` is the randomness of the commitment. eg if `m` = 325 and `m` is decomposed in 4-bit chunks, `b` is 16 (2^4) and decomposition is [1, 4, 5] as `325 = 1 * 16^2 + 4 * 16^1 + 5 * 16^0`.


#### Getting a commitment to the full message from commitment to the decomposition.

An inefficient (insecure as well?) way to get a commitment `m*G + r'*H` from `phi` is to create a commitment `J` as:

```
J = m_1*G_1 + m_2*G_2 + ... + m_n*G_n + r'*H  
```

where `G_i = {b^{n-i}}*G` so `G_1 = {b^{n-1}}*G`, and so on.  
Now prove the equality of openings of the commitments `phi` and `J`. Note that `J` is same as `m*G + r'*H` because

```
m_1*G_1 + m_2*G_2 + ... + m_n*G_n + r'*H 
  = m_1*{b^{n-1}}*G + m_2*{b^{n-2}}*G + ... + m_n*G + r'*H  
  = ( m_1*{b^{n-1}} + m_2*{b^{n-2}} + ... + m_n ) * G + r'*H 
  = m*G + r'*H
```

Since `b`, `n` and `G` are public, it can be ensured that `G_i`s are correctly created.


### Still some TODOs pending