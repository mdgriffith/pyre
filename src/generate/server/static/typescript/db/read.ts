import { Value } from 'json-typescript'

// Types
type Id = number;

interface IdField<T> {
    name: string;
    decoder: (value: Value) => T;
}

type Decoder<T> = {
    decode: (index: number, json: Value) => T;
};

interface Query<T> {
    identity: Array<(value: Value) => Id>;
    decoder: Decoder<T>;
}

// Time utilities (simplified Posix time handling)
const Time = {
    millisToPosix: (ms: number): Date => new Date(ms)
};

// Core decoder functions
export function succeed<T>(value: T): Decoder<T> {
    return {
        decode: (_, __) => value
    };
}

export function nullable<T>(decoder: Decoder<T>): Decoder<T | null> {
    return {
        decode: (index: number, json: Value) => {
            try {
                return decoder.decode(index, json);
            } catch {
                return null;
            }
        }
    };
}

export function custom<T>(decoder: (value: Value) => T): Decoder<T> {
    return {
        decode: (_, json) => decoder(json)
    };
}

// Primitive decoders
export const int: Decoder<number> = {
    decode: (_, json) => {
        if (typeof json !== 'number' || !Number.isInteger(json)) {
            throw new Error('Expected integer');
        }
        return json;
    }
};

export const dateTime: Decoder<Date> = {
    decode: (_, json) => {
        if (typeof json !== 'number') {
            throw new Error('Expected number for datetime');
        }
        return Time.millisToPosix(json * 1000);
    }
};

export const string: Decoder<string> = {
    decode: (_, json) => {
        if (typeof json !== 'string') {
            throw new Error('Expected string');
        }
        return json;
    }
};

export const bool: Decoder<boolean> = {
    decode: (_, json) => {
        if (typeof json === 'boolean') return json;
        if (typeof json === 'number') return json !== 0;
        throw new Error('Expected boolean or number');
    }
};

export const float: Decoder<number> = {
    decode: (_, json) => {
        if (typeof json !== 'number') {
            throw new Error('Expected number');
        }
        return json;
    }
};

// Query construction
export function query<T>(decoder: T, identity: Array<IdField<Id>>): Query<T> {
    return {
        identity: identity.map(field => field.decoder),
        decoder: succeed(decoder)
    };
}

export function field<T, U>(
    fieldName: string,
    fieldDecoder: Decoder<T>,
    query: Query<(value: T) => U>
): Query<U> {
    return {
        identity: query.identity,
        decoder: {
            decode: (index: number, json: Value) => {
                const builder = query.decoder.decode(index, json);
                const value = fieldDecoder.decode(index, (json as any)[fieldName]);
                return builder(value);
            }
        }
    };
}

// ID handling
export function id(name: string): IdField<Id> {
    return {
        name,
        decoder: (json: Value) => {
            const value = (json as any)[name];
            if (typeof value !== 'number' || !Number.isInteger(value)) {
                throw new Error(`Expected integer for ID field ${name}`);
            }
            return value;
        }
    };
}

// Nested queries
export function nested<Inner, Outer>(
    topLevelIdField: IdField<Id>,
    innerId: IdField<Id>,
    innerQuery: Query<Inner>,
    topQuery: Query<(inner: Inner[]) => Outer>
): Query<Outer> {
    return {
        identity: topQuery.identity,
        decoder: {
            decode: (topLevelIndex: number, fullJson: Value) => {
                try {
                    const parentId = topLevelIdField.decoder(fullJson);
                    const innerResults = decodeValue(innerQuery, fullJson);
                    const builder = topQuery.decoder.decode(topLevelIndex, fullJson);
                    return builder(innerResults);
                } catch {
                    const builder = topQuery.decoder.decode(topLevelIndex, fullJson);
                    return builder([]);
                }
            }
        }
    };
}

// Main decode functions
export function decodeValue<T>(query: Query<T>, json: Value): T[] {
    return runDecoderWith(0, query.identity, query.decoder, () => true, json);
}

function runDecoderWith<T>(
    startingIndex: number,
    uniqueBy: Array<(value: Value) => Id>,
    decoder: Decoder<T>,
    rowCheck: () => boolean,
    json: Value
): T[] {
    const found = new Set<string>();
    const results: T[] = [];

    let index = startingIndex;
    while (true) {
        try {
            if (!rowCheck()) break;

            const compoundId = uniqueBy
                .map(decoder => decoder(json).toString())
                .join('_');

            if (!found.has(compoundId) || compoundId === '') {
                const result = decoder.decode(index, json);
                results.push(result);
                found.add(compoundId);
            }

            index++;
        } catch {
            break;
        }
    }

    return results.reverse();
}
