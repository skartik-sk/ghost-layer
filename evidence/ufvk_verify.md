# `ghost verify --ufvk`: read-only viewing-key check (real testnet, 2026-11-04T14:58:35Z)

Viewing key of our demo **receiving** wallet (testnet only; a UFVK can decrypt, never spend):

```
uviewtest178s9e86r028vu76wtypt3ufpprthn9enn7q00zw8teg89hgq8dzclvede59nhvg6jhl3j6sncku87nvuqqg2kw7sqrwcjcm5luag7hjtnlfyevtzgps37s5e24rf64pwafepprdw3dg287rx37ahhzhhty8zw0xjjyn4cswm4llf6wry4w8zu8vdarjpyvsxsv26tvhduvkn9h3gf9vxduxcr0tygtmu9mp0wpuk2t7njr6t7488z6mdtjrzc7y73vk6sa822m0nkgu8x36vlshenjwlte6y2n8xcfejaswd0edxp2kcwdr9jx7q7t8s9n6v3c8u2ryp02a5gv73vnflm6k2qcw2guzvkmh8rw250cuplylv8synavwjlsw7u0nw9ran9qz97zpdfgs0glme2z2cltnjmhnfshc7vlm23xw6xxywjzgqcmxkfzes2j4sezmzx94v27vzcs3ms822nac4fmjywkph4nfwvzxarwgwkqae2fke
```

Decryption uses librustzcash @ eb38d7a `decrypt_transaction`; Ironwood (NU7) outputs are Orchard-shaped and read via `ironwood_outputs()`.

`ghost verify --ufvk <key> 09f0f0b01942a658197f65563a977c66fc19f8823268b0e547470e022bd3ce69`
```
✅ mined at height 4582118 — 287 confirmation(s) (tip 4582404)
   txid 09f0f0b01942a658197f65563a977c66fc19f8823268b0e547470e022bd3ce69
   🔗 https://testnet.zcashexplorer.app/transactions/09f0f0b01942a658197f65563a977c66fc19f8823268b0e547470e022bd3ce69
🔑 ironwood incoming — 0.00100000 TAZ (100000 zat), output #0, memo: (empty)
```

`ghost verify --ufvk <key> 4d8f96ec56cd36871d6611e8fbe72cfc37cd7e9730d32cafeb2992fa126b4970`
```
✅ mined at height 4582106 — 300 confirmation(s) (tip 4582405)
   txid 4d8f96ec56cd36871d6611e8fbe72cfc37cd7e9730d32cafeb2992fa126b4970
   🔗 https://testnet.zcashexplorer.app/transactions/4d8f96ec56cd36871d6611e8fbe72cfc37cd7e9730d32cafeb2992fa126b4970
🔑 ironwood incoming — 0.00100000 TAZ (100000 zat), output #1, memo: (empty)
```

`ghost verify --ufvk <key> 2382433aeeed74c7ace9f6bd65738a4d11148a1460165c00082fa56b6e25177d`
```
✅ mined at height 4582374 — 32 confirmation(s) (tip 4582405)
   txid 2382433aeeed74c7ace9f6bd65738a4d11148a1460165c00082fa56b6e25177d
   🔗 https://testnet.zcashexplorer.app/transactions/2382433aeeed74c7ace9f6bd65738a4d11148a1460165c00082fa56b6e25177d
🔑 ironwood outgoing — 0.00010000 TAZ (10000 zat), output #0, memo: (empty)
🔑 ironwood change — 0.00080000 TAZ (80000 zat), output #1, memo: (empty)
```

`ghost verify --ufvk <key> 14434aabc6b892b7201e5f3f80ad0be12392bae628fcc75dcd4b12effa8eb3a5`
```
✅ mined at height 4581966 — 440 confirmation(s) (tip 4582405)
   txid 14434aabc6b892b7201e5f3f80ad0be12392bae628fcc75dcd4b12effa8eb3a5
   🔗 https://testnet.zcashexplorer.app/transactions/14434aabc6b892b7201e5f3f80ad0be12392bae628fcc75dcd4b12effa8eb3a5
🔑 viewing key: no outputs of this tx belong to that wallet
```

- `09f0f0b0…` (after, via Ghost Layer + Nym) and `4d8f96ec…` (before): the receiving wallet sees **0.001 TAZ incoming, Ironwood**.
- `2382433a…` (sent *by* this wallet in the queue-mode test): outgoing 0.0001 + change.
- `14434aab…` (an unrelated mined tx): negative control, nothing decrypts.
