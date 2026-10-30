## Data Model

Below is a diagram of the data model used for the chaincash server.

```mermaid
erDiagram
    NOTE {
        int id PK
        int box_id FK
        int denomination_id FK
        int value "The value of the note in its denomination, this is the amount of tokens in tokens(0)"
        string owner "Hex encoded public key of the current owner"
    }
    RESERVE {
        int id PK
        int box_id FK
        int denomination_id FK
        string owner "Hex encoded public key of the owner of the reserves"
    }
    ERGO_BOX {
        int id PK
        string[32] ergo_id "Modifier id of the box on the Ergo network"
        byte[] bytes "Serialized box bytes"
    }
    OWNERSHIP_ENTRY {
        int id PK
        int note_id FK
        int amount "Maximum amount that can be redeemed from reserve"
        int position "Index of signature in history"
        string[32] reserve_nft_id "Reserve NFT id used as the key for the signed data inserted into the ergo box avltree"
        byte[] signature "signature"
    }
    DENOMINATION {
        int id PK
        int type "Type enum of the denomination, 0 = erg, 1 = gold"
        int nanoerg_per_unit "The conversion rate of this denomination in nanoergs"
    }
    REFUND {
        int id PK
        string reserve_id "Singleton NFT id of the refunded reserve"
        string reserve_box_id "Reserve box id carrying or settling the refund"
        int requested_amount "Maximum nanoErgs announced for withdrawal in R6"
        int refunded_amount "Actual nanoErgs withdrawn once completed"
        int init_height "Initiation block height recorded in R5"
        int settled_height "Block height when completed or cancelled"
        string status "pending, executing, completed, cancelled, or failed"
        string init_tx_id "Transaction id that initiated the refund"
        string refund_tx_id "Transaction id that completed or cancelled the refund"
        string error_message "Diagnostic failure reason when failed or retrying"
    }
    NOTE ||--|{ OWNERSHIP_ENTRY : "has"
    NOTE ||--|| DENOMINATION : "has"
    NOTE ||--|| ERGO_BOX : "is a"
    RESERVE ||--|| ERGO_BOX : "is a"
```
