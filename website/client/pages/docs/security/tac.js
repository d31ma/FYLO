const TITLE = 'Encryption & access — FYLO'
document.title = TITLE

export default class {
    schemaCode = `{
    "$encrypted": ["ssn", "email", "payload/verifier"],
    "id": "^[0-9]+$",
    "name": "^.+$",
    "email": "^.+$",
    "ssn?": "^[0-9-]+$",
    "payload?": { "verifier?": "^.+$" }
}`

    identityCode = `fylo identity create --store file:admin.pem
fylo identity create --store file:app.pem
fylo identity show file:app.pem --json
fylo create app.vdbx --admin file:admin.pem`

    grantCode = `fylo access grant app.vdbx --grantee web --key fpk1:p256:... \\
    --actions read --collection users \\
    --identity file:admin.pem --trust-anchor fid1:p256:...
fylo access list app.vdbx --identity file:app.pem --trust-anchor fid1:p256:...`

    configCode = `{
    "fylo": {
        "root": "./app.vdbx",
        "identity": "file:app.pem",
        "trustAnchor": "fid1:p256:..."
    }
}`

    migrateCode = `fylo access report ./old-root
fylo pack ./old-root --out app.vdbx --protect --admin file:admin.pem --drop-record-access`
}
