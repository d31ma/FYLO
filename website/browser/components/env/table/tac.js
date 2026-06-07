export default class extends Tac {
  envRows = [
    {
      key: 'FYLO_ROOT',
      kind: 'plain',
      text: 'Filesystem root for FYLO collections. Defaults to .fylo-data in the current working directory.',
    },
    {
      key: 'FYLO_SCHEMA',
      kind: 'plain',
      text: 'Directory containing collection JSON schemas for validation and encryption metadata.',
    },
    {
      key: 'FYLO_STRICT',
      kind: 'plain',
      text: 'Set to any truthy value to validate writes with CHEX before they are committed.',
    },
    {
      key: 'FYLO_ENCRYPTION_KEY',
      kind: 'code-trailing',
      textBefore: 'Minimum 32 characters. Required when any schema declares ',
      code: '$encrypted',
      textAfter: ' fields.',
    },
    {
      key: 'FYLO_CIPHER_SALT',
      kind: 'plain',
      text: 'Unique random salt per deployment for PBKDF2 key derivation. Prevents cross-instance precomputation.',
    },
    {
      key: 'FYLO_LOGGING',
      kind: 'plain',
      text: 'Enable extra logging for bulk imports and related FYLO operations.',
    },
  ]
}
