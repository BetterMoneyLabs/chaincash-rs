# ChainCash Server

[![ci](https://github.com/ChainCashLabs/chaincash-rs/actions/workflows/ci.yaml/badge.svg?branch=master)](https://github.com/ChainCashLabs/chaincash-rs/actions/workflows/ci.yaml) [![audit](https://github.com/ChainCashLabs/chaincash-rs/actions/workflows/audit.yaml/badge.svg)](https://github.com/ChainCashLabs/chaincash-rs/actions/workflows/audit.yaml)
[![Discord badge][]][Discord link] [![Telegram](https://img.shields.io/badge/Telegram-2CA5E0?style=flat-squeare&logo=telegram&logoColor=white)](https://t.me/chaincashtalks)

`ChainCash` is a p2p monetary system with elastic money creation backed by trust and blockchain assets.

For in-depth explanation please refer to the whitepaper [here](https://github.com/ChainCashLabs/chaincash/blob/master/docs/whitepaper/chaincash.pdf).

This repository contains offchain/server software for agents participating in `ChainCash`. Running ChainCash server 
allows you to run your own bank in ChainCash free-banking network, which works on top of Ergo blockchain. With your own
server you can set your own acceptance predicate (filter for notes you accept), create your own reserve on the blockchain 
and issue notes against it. 


## Running

We aren't yet shipping prebuilt `ChainCash` binaries so currently you must build from source yourself using `cargo`.

Firstly ensure that you have the `contracts` submodule initialized:

```sh
git submodule update --init
```

The easiest way to build and run currently is like so:

```sh
cargo run -- run
```

This will start the `ChainCash` server and initialize the database, etc.

## Configuration

`ChainCash` is configured using `TOML` config files stored in `$CWD/config`.

The default settings can be viewed in [`./config/default.toml`](./config/default.toml).

Default config values can be overriden by creating your own config file at [`./config/local.toml`] and supplying custom values.

## Predicates

### Predicate Configuration

Predicates are configured using `TOML` based config files. The structure of these config files are as follows:

```toml
type = "{predicateType}" # the type of predicate
{...args} # `args` is unique to the predicate type and are listed below
```

To enable a predicate config we specify it in our main `ChainCash` configuration file under the `acceptance` section:

```toml
[acceptance]
predicates = ["path/to/my/predicate1.toml", "path/to/my/predicate2.toml"]
```

If any of the predicates listed in the `predicates` field evaluate to `true` for a given note then the note will be considered acceptable.

Example of a predicate config file can be seen [here](./config/predicates/example.toml).

### Viewing Configured Predicates

With the `ChainCash` server running locally we can perform an API request to get our predicate configuration.

```sh
curl http://localhost:8080/api/v1/acceptance
```

To check whether a JSON encoded note can be accepted by the configured predicates, post it to the check endpoint:

```sh
curl -X POST http://localhost:8080/api/v1/acceptance/checkNote \
  -H "content-type: application/json" \
  -d '{"nanoerg":1000,"owner":"owner1","issuer":"issuer1","signers":["issuer1"]}'
```

The response contains a boolean `accepted` field.

### Predicate Types

Currently the following predicates are supported:

#### Whitelist

A `whitelist` predicate evaluates to `true` if any of the suppplied agents match depending on the `kind` field.

`whitelist` has subtypes defined in the `kind` field, the following are supported:

- `owner` whitelist is based on the current note holder
- `issuer` whitelist is based on the note issuer
- `historical` whitelist checks each holder of the note

If the owner is known to us and trusted we can accept the note without any other consideration.

For example, this could be configured like so:

```toml
type = "whitelist"
kind = "owner"
agents = ["030c8f9c4dc08f3c006fa85a47c9156dedbede000a8b764c6e374fd097e873ba04", "0216133993bbc54c0d48a21634a7d2632b8c92d744d565839dc39c912ef406e0d9"]
```

`agents` here are public keys provided as encoded elliptic curve points. To obtain public key in such form from an 
Ergo address, you can use `utils/addressToRaw` API method of an Ergo node, for example:

```shell
curl -X 'GET' \
  'http://213.239.193.208:9053/utils/addressToRaw/9egnPnrYskFS8k1gYiKZEXZ2bhP9fvX9GZvsG1V3BzH3n8sBXrf' \
  -H 'accept: application/json'
```

or use Swagger interface: [http://213.239.193.208:9053/swagger#/utils/AddressToRaw](http://213.239.193.208:9053/swagger#/utils/AddressToRaw).

#### Blacklist

A `blacklist` predicate evaluates to `true` if none of the suppplied agents match depending on the `kind` field.

`blacklist` has subtypes defined in the `kind` field, the following are supported:

- `owner` blacklist is based on the current note holder
- `issuer` blacklist is based on the note issuer
- `historical` blacklist checks each holder of the note

If we want to blacklist all notes issued by `PK1` this could be done like so:

```toml
type = "blacklist"
kind = "issuer"
agents = ["0216133993bbc54c0d48a21634a7d2632b8c92d744d565839dc39c912ef406e0d9", "030c8f9c4dc08f3c006fa85a47c9156dedbede000a8b764c6e374fd097e873ba04"]
```

#### Collateral

A `collateral` predicate evaluates to `true` if the note is backed by at least the `percent` supplied.

For example, we could require the note be over-collaterized if we really don't trust the owner, or under collaterized if we have _some_ trust in the holder.

Configuration of a note that requires at least 100% collateral.

```toml
type = "collateral"
percent = 100
```


#### Or

An `or` predicate evaluates to `true` if any of the conditions supplied evalute to `true`.

For example, if we want to express that a note is accepted if the note is over collaterized or the owner is trusted by us we could do the following:

```toml
# any of the conditions below match
type = "or"
conditions = [
    # the owner of the note is either PK1 or PK2
    {type = "whitelist", kind = "owner", agents = ["030c8f9c4dc08f3c006fa85a47c9156dedbede000a8b764c6e374fd097e873ba04"]},
    # the note has at least 100% collateral
    {type = "collateral", percent = 100}
]
```

## API

Following API methods are supported now (default URLs are provided, if you changed IP address or port, update URLs 
accordingly):

* Mint reserve ( `http://127.0.0.1:8080/api/v1/reserves/mint` )

send JSON via POST method like 
```json
{
   "public_key_hex": "$pubkeyHex",
   "amount": 1000000
}
``` 

where `$pubkeyHex` is your public key provided as encoded elliptic curve point (see how to get it above) , 
and amount is in nanoErgs (0.001 ERG in this example)

result would be like
```json
{"txId":"d2ccfce5c267d0f0fb51750d47ee966168611e40374e65df31aafba3abd954ef","reserveNftId":"0f44aa54140dbd5368b44358630d5ca4e38e6405f76bd987e18d7eae667915db"}
```

so transaction id and reserve NFT id which will be used further to identify the reserve

* Get known reserves ( `http://127.0.0.1:8080/api/v1/reserves` - GET method )

* Mint note ( `http://127.0.0.1:8080/api/v1/notes/mint` )

send JSON via POST method like
```json
{
  "owner_public_key_hex": "$pubkeyHex",
  "gold_amount_mg": 1000
}
``` 
where `$pubkeyHex` is your public key, and `gold_amount_mg` is note value in milligrams of gold (1 gram in our example)

* List notes you posses ( `http://127.0.0.1:8080/api/v1/notes/wallet` - GET method)

* Spend a note ( `http://127.0.0.1:8080/api/v1/notes/spend` )

send JSON via POST method like

```json
 {
   "note_id": 1,
   "reserve_id": "0f44aa54140dbd5368b44358630d5ca4e38e6405f76bd987e18d7eae667915db",
   "recipient_pubkey": "02b8466784b34d5393a46b789f27b66f7fd34e1a06faf0d7941e204d71ead6ccdd",
   "amount": 50
 }
```

where note_id (the only new parameter in this request) is note id taken from results of previous API 
method (`/notes/wallet`)

* List notes possessed by a public key ( `http://127.0.0.1:8080/api/v1/notes/byPubkey/:pubkey` - GET method )

* Redeem a note ( `http://127.0.0.1:8080/api/v1/notes/redeem` )

send JSON via POST method like

{
    "note_id": 1,
    "reserve_id": "0f44aa54140dbd5368b44358630d5ca4e38e6405f76bd987e18d7eae667915db"
}

* Get note acceptance rules ( `http://127.0.0.1:8080/api/v1/acceptance/` )

[Discord badge]: https://img.shields.io/discord/668903786361651200?logo=discord&style=social
[Discord link]: https://discord.gg/ergo-platform-668903786361651200
