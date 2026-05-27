/**
 * @typedef {object} JoinOperand
 * @property {string=} $eq
 * @property {string=} $ne
 * @property {string=} $gt
 * @property {string=} $lt
 * @property {string=} $gte
 * @property {string=} $lte
 */

/**
 * @typedef {object} TimestampQuery
 * @property {number=} $gt
 * @property {number=} $lt
 * @property {number=} $gte
 * @property {number=} $lte
 */

/**
 * @typedef {object} Operand
 * @property {*=} $eq
 * @property {*=} $ne
 * @property {number=} $gt
 * @property {number=} $lt
 * @property {number=} $gte
 * @property {number=} $lte
 * @property {string=} $like
 * @property {string | number | boolean=} $contains
 */

/**
 * @template {Record<string, any>} T
 * @typedef {Partial<Record<keyof T, Operand>>} QueryOperation
 */

/**
 * @template {Record<string, any>} T
 * @template {Record<string, any>} U
 * @typedef {Partial<Record<keyof T, JoinOperand>>} JoinCondition
 */

/**
 * @template {Record<string, any>} T
 * @template {Record<string, any>} U
 * @typedef {object} StoreJoin
 * @property {Array<keyof T | keyof U>=} $select
 * @property {string} $leftCollection
 * @property {string} $rightCollection
 * @property {'inner' | 'left' | 'right' | 'outer'} $mode
 * @property {JoinCondition<T, U>} $on
 * @property {number=} $limit
 * @property {boolean=} $onlyIds
 * @property {keyof T | keyof U=} $groupby
 * @property {Record<string, string>=} $rename
 */

/**
 * @template {Record<string, any>} T
 * @typedef {object} StoreQuery
 * @property {Array<keyof T>=} $select
 * @property {Record<string, string>=} $rename
 * @property {string=} $collection
 * @property {Array<QueryOperation<T>>=} $ops
 * @property {number=} $limit
 * @property {boolean=} $onlyIds
 * @property {keyof T=} $groupby
 * @property {TimestampQuery=} $updated
 * @property {TimestampQuery=} $created
 * @property {TimestampQuery=} $deleted
 */

/**
 * @typedef {object} SqlCondition
 * @property {string} column
 * @property {string} operator
 * @property {string | number | boolean | null} value
 */

/**
 * @template {Record<string, any>} T
 * @typedef {object} StoreUpdate
 * @property {string=} $collection
 * @property {StoreQuery<T>=} $where
 * @property {{ [K in keyof Partial<T>]: T[K] }} $set
 */

/**
 * @template {Record<string, any>} T
 * @typedef {StoreQuery<T>} StoreDelete
 */

/**
 * @template {Record<string, any>} T
 * @typedef {object} StoreInsert
 * @property {string=} $collection
 * @property {{ [K in keyof T]: T[K] }} $values
 */

export {}
