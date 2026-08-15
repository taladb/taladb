import { describe, it, expect, vi } from 'vitest';
import { applySchema } from '../src/index';
import type { Collection, Document } from '../src/index';

// Read-time document migration (CollectionOptions.migrateDocument): a lazy,
// arbitrary-JS normalization applied to documents returned by find/findOne when
// their `_v` is below syncSchema.version. Runtime-agnostic — pure client
// transform — so it's tested here against a stub collection, no engine needed.

interface UserDoc extends Document {
  first?: string;
  last?: string;
  fullName?: string;
}

function stub(docs: UserDoc[]): Collection<UserDoc> {
  return {
    insert: async () => 'id',
    insertMany: async () => ['id'],
    find: async () => [...docs],
    findOne: async () => docs[0] ?? null,
    updateOne: async () => true,
    updateMany: async () => 0,
    deleteOne: async () => true,
    deleteMany: async () => 0,
    replaceManyWithIds: async (rows) => rows.map((row) => row._id),
    deleteManyWithIds: async () => 0,
    count: async () => docs.length,
    aggregate: async () => [],
    createIndex: async () => {},
    dropIndex: async () => {},
    createCompoundIndex: async () => {},
    dropCompoundIndex: async () => {},
    createFtsIndex: async () => {},
    dropFtsIndex: async () => {},
    listIndexes: async () => ({ btree: [], fts: [], vector: [] }),
    createVectorIndex: async () => {},
    dropVectorIndex: async () => {},
    upgradeVectorIndex: async () => {},
    findNearest: async () => [],
    subscribe: () => () => {},
    subscribeAggregate: () => () => {},
  };
}

const migrate = (doc: UserDoc, from: number): UserDoc =>
  from < 2 ? { ...doc, fullName: `${doc.first ?? ''} ${doc.last ?? ''}`.trim() } : doc;

describe('read-time migrateDocument', () => {
  it('upgrades a below-version document on find and stamps _v', async () => {
    const col = applySchema(stub([{ first: 'Ada', last: 'Lovelace' }]), {
      syncSchema: { version: 2 },
      migrateDocument: migrate,
    });
    const [doc] = await col.find();
    expect(doc.fullName).toBe('Ada Lovelace'); // computed from old shape
    expect(doc._v).toBe(2); // stamped to target
  });

  it('leaves an at-version document untouched (migrate not called)', async () => {
    let called = false;
    const col = applySchema(stub([{ _v: 2, first: 'Grace', fullName: 'Grace Hopper' }]), {
      syncSchema: { version: 2 },
      migrateDocument: (d, f) => {
        called = true;
        return migrate(d, f);
      },
    });
    const [doc] = await col.find();
    expect(called).toBe(false);
    expect(doc.fullName).toBe('Grace Hopper');
  });

  it('applies on findOne too', async () => {
    const col = applySchema(stub([{ first: 'Alan', last: 'Turing' }]), {
      syncSchema: { version: 2 },
      migrateDocument: migrate,
    });
    const doc = await col.findOne({});
    expect(doc?.fullName).toBe('Alan Turing');
    expect(doc?._v).toBe(2);
  });

  it('treats a missing _v as version 0', async () => {
    const seen: number[] = [];
    const col = applySchema(stub([{ first: 'x' }]), {
      syncSchema: { version: 3 },
      migrateDocument: (d, from) => {
        seen.push(from);
        return d;
      },
    });
    await col.find();
    expect(seen).toEqual([0]);
  });

  it('throws when migrateDocument is set without syncSchema.version', () => {
    expect(() =>
      applySchema(stub([]), { migrateDocument: (d) => d }),
    ).toThrow('requires syncSchema.version');
  });

  it('works with no schema present (read-only migration)', async () => {
    // No Zod schema — migrateDocument alone still wraps reads.
    const col = applySchema(stub([{ first: 'a', last: 'b' }]), {
      syncSchema: { version: 2 },
      migrateDocument: migrate,
    });
    const [doc] = await col.find();
    expect(doc.fullName).toBe('a b');
  });
});

describe('persistMigrations (persist-on-read)', () => {
  /** A stub whose `updateOne` is a spy, so we can assert the write-back diff. */
  function persistStub(docs: UserDoc[]) {
    const updateOne = vi.fn(async () => true);
    const base = stub(docs);
    return { col: { ...base, updateOne } as Collection<UserDoc>, updateOne };
  }

  it('writes the upgraded shape back via updateOne when enabled', async () => {
    const { col, updateOne } = persistStub([{ _id: 'u1', first: 'Ada', last: 'Lovelace' }]);
    const wrapped = applySchema(col, {
      syncSchema: { version: 2 },
      migrateDocument: migrate,
      persistMigrations: true,
    });
    const [doc] = await wrapped.find();
    expect(doc.fullName).toBe('Ada Lovelace');
    expect(updateOne).toHaveBeenCalledTimes(1);
    const [filter, update] = updateOne.mock.calls[0] as unknown as [unknown, Record<string, unknown>];
    expect(filter).toEqual({
      $and: [{ _id: 'u1' }, { _v: { $exists: false } }],
    });
    expect(update.$set).toMatchObject({ fullName: 'Ada Lovelace', _v: 2 });
  });

  it('guards write-back with the stored version and change timestamp', async () => {
    const { col, updateOne } = persistStub([
      { _id: 'u1', _v: 1, _changed_at: 42, first: 'Ada', last: 'Lovelace' },
    ]);
    const wrapped = applySchema(col, {
      syncSchema: { version: 2 },
      migrateDocument: migrate,
      persistMigrations: true,
    });
    await wrapped.find();
    expect(updateOne.mock.calls[0]?.[0]).toEqual({
      $and: [{ _id: 'u1' }, { _v: 1 }, { _changed_at: 42 }],
    });
  });

  // A migration that drops a field is, by default, treated as *not knowing about*
  // that field rather than as intending to delete it — because on a synced
  // collection those two are indistinguishable from the migration's output, and
  // guessing "delete" propagates a newer peer's field into oblivion under LWW.
  const renameV2toV3 = (d: UserDoc): UserDoc => {
    const { fullName, ...rest } = d as UserDoc & { fullName?: string };
    return { ...rest, name: fullName } as UserDoc;
  };

  it('write-back is additive-only: a dropped field is not $unset by default', async () => {
    const { col, updateOne } = persistStub([{ _id: 'u1', _v: 2, fullName: 'Ada L' } as UserDoc]);
    const wrapped = applySchema(col, {
      syncSchema: { version: 3 },
      migrateDocument: renameV2toV3,
      persistMigrations: true,
    });
    await wrapped.find();
    const [, update] = updateOne.mock.calls[0] as unknown as [unknown, Record<string, unknown>];
    expect(update.$set).toMatchObject({ name: 'Ada L', _v: 3 });
    expect(update.$unset).toBeUndefined(); // fullName survives in storage
  });

  it('$unset removes fields dropped by the migration under allowFieldRemoval', async () => {
    const { col, updateOne } = persistStub([{ _id: 'u1', _v: 2, fullName: 'Ada L' } as UserDoc]);
    const wrapped = applySchema(col, {
      syncSchema: { version: 3 },
      migrateDocument: renameV2toV3,
      persistMigrations: true,
      allowFieldRemoval: true,
    });
    await wrapped.find();
    const [, update] = updateOne.mock.calls[0] as unknown as [unknown, Record<string, unknown>];
    expect(update.$set).toMatchObject({ name: 'Ada L', _v: 3 });
    expect(update.$unset).toEqual({ fullName: true });
  });

  it('removes only explicitly retired fields', async () => {
    const { col, updateOne } = persistStub([
      { _id: 'u1', _v: 2, fullName: 'Ada L', futureField: 'keep' } as UserDoc,
    ]);
    const wrapped = applySchema(col, {
      syncSchema: { version: 3 },
      migrateDocument: renameV2toV3,
      persistMigrations: true,
      retiredFields: ['fullName'],
    });
    await wrapped.find();
    const [, update] = updateOne.mock.calls[0] as unknown as [unknown, Record<string, unknown>];
    expect(update.$unset).toEqual({ fullName: true });
    expect((update.$unset as Record<string, unknown>).futureField).toBeUndefined();
  });

  it('does not write when persistMigrations is off', async () => {
    const { col, updateOne } = persistStub([{ _id: 'u1', first: 'a', last: 'b' }]);
    const wrapped = applySchema(col, {
      syncSchema: { version: 2 },
      migrateDocument: migrate,
    });
    await wrapped.find();
    expect(updateOne).not.toHaveBeenCalled();
  });

  it('does not write for an already-current document', async () => {
    const { col, updateOne } = persistStub([{ _id: 'u1', _v: 2, fullName: 'x' }]);
    const wrapped = applySchema(col, {
      syncSchema: { version: 2 },
      migrateDocument: migrate,
      persistMigrations: true,
    });
    await wrapped.find();
    expect(updateOne).not.toHaveBeenCalled();
  });

  it('is best-effort: a failed write-back still returns the migrated value', async () => {
    const base = stub([{ _id: 'u1', first: 'Alan', last: 'Turing' }]);
    const col = { ...base, updateOne: vi.fn(async () => { throw new Error('disk full'); }) } as Collection<UserDoc>;
    const wrapped = applySchema(col, {
      syncSchema: { version: 2 },
      migrateDocument: migrate,
      persistMigrations: true,
    });
    const [doc] = await wrapped.find();
    expect(doc.fullName).toBe('Alan Turing'); // returned despite the write failing
  });

  it('does not rewrite when only nested key order differs', async () => {
    // The migration rebuilds `address` with the same values in a different key
    // order. A JSON.stringify comparison would see a difference and rewrite the
    // document on every single read; a structural compare correctly sees none.
    const stored = { _id: 'u1', _v: 1, address: { street: 'A', city: 'Manila' } } as UserDoc;
    const { col, updateOne } = persistStub([stored]);
    const wrapped = applySchema(col, {
      syncSchema: { version: 2 },
      migrateDocument: (d) => ({
        ...d,
        address: { city: 'Manila', street: 'A' },
      }),
      persistMigrations: true,
    });
    await wrapped.find();
    const [, update] = updateOne.mock.calls[0] as unknown as [unknown, Record<string, unknown>];
    expect(update.$set).toEqual({ _v: 2 }); // only the version bump, not `address`
  });
});

// ---------------------------------------------------------------------------
// `_v` stamping on insert
// ---------------------------------------------------------------------------
// Without this, a locally-inserted document reads back with no `_v`, counts as
// version 0, and gets fed through migrateDocument as if it were legacy data —
// corrupting brand-new documents written in the current shape.

describe('_v stamping on insert', () => {
  function insertStub() {
    const inserted: UserDoc[] = [];
    const base = stub([]);
    const col = {
      ...base,
      insert: async (doc: UserDoc) => { inserted.push(doc); return 'id'; },
      insertMany: async (docs: UserDoc[]) => { inserted.push(...docs); return ['id']; },
    } as unknown as Collection<UserDoc>;
    return { col, inserted };
  }

  it('stamps the current syncSchema.version on insert and insertMany', async () => {
    const { col, inserted } = insertStub();
    const wrapped = applySchema(col, { syncSchema: { version: 2 } });
    await wrapped.insert({ fullName: 'Ada Lovelace' });
    await wrapped.insertMany([{ fullName: 'Alan Turing' }]);
    expect(inserted.map((d) => d._v)).toEqual([2, 2]);
  });

  it('preserves an explicitly supplied _v', async () => {
    const { col, inserted } = insertStub();
    const wrapped = applySchema(col, { syncSchema: { version: 3 } });
    await wrapped.insert({ fullName: 'x', _v: 1 } as UserDoc);
    expect(inserted[0]._v).toBe(1);
  });

  it('does not stamp when no syncSchema.version is declared', async () => {
    const { col, inserted } = insertStub();
    const wrapped = applySchema(col, { syncSchema: { required: ['fullName'] } });
    await wrapped.insert({ fullName: 'x' });
    expect(inserted[0]._v).toBeUndefined();
  });

  it('does not run migrateDocument over a freshly-inserted document', async () => {
    // The regression: insert a current-shape doc, read it back, and the v1→v2
    // migration would run on it — producing `fullName: "undefined undefined"`.
    const inserted: UserDoc[] = [];
    const base = stub([]);
    const col = {
      ...base,
      insert: async (doc: UserDoc) => { inserted.push(doc); return 'id'; },
      find: async () => [...inserted],
    } as unknown as Collection<UserDoc>;

    const seen: number[] = [];
    const wrapped = applySchema(col, {
      syncSchema: { version: 2 },
      migrateDocument: (d, from) => {
        seen.push(from);
        return { ...d, fullName: `${d.first} ${d.last}` };
      },
    });
    await wrapped.insert({ fullName: 'Ada Lovelace' });
    const [doc] = await wrapped.find();

    expect(seen).toEqual([]); // migration never consulted
    expect(doc.fullName).toBe('Ada Lovelace'); // not "undefined undefined"
  });
});

// ---------------------------------------------------------------------------
// Live queries (subscribe) — the path every @taladb/react hook reads through
// ---------------------------------------------------------------------------

describe('subscribe (live queries)', () => {
  /** A stub whose `subscribe` hands us the engine-side callback to fire. */
  function subscribeStub(docs: UserDoc[]) {
    let emit: ((docs: UserDoc[]) => void) | null = null;
    const updateOne = vi.fn(async () => true);
    const base = stub(docs);
    const col = {
      ...base,
      updateOne,
      subscribe: (_f: unknown, cb: (d: UserDoc[]) => void) => { emit = cb; return () => {}; },
    } as unknown as Collection<UserDoc>;
    return { col, updateOne, fire: (d: UserDoc[]) => emit!(d) };
  }

  it('delivers migrated documents to the subscriber', async () => {
    const { col, fire } = subscribeStub([]);
    const wrapped = applySchema(col, {
      syncSchema: { version: 2 },
      migrateDocument: migrate,
    });
    const received: UserDoc[][] = [];
    wrapped.subscribe({}, (docs) => received.push(docs));
    fire([{ _id: 'u1', first: 'Ada', last: 'Lovelace' }]);

    expect(received[0][0].fullName).toBe('Ada Lovelace');
    expect(received[0][0]._v).toBe(2);
  });

  it('persists the upgraded shape when persistMigrations is on', async () => {
    const { col, updateOne, fire } = subscribeStub([]);
    const wrapped = applySchema(col, {
      syncSchema: { version: 2 },
      migrateDocument: migrate,
      persistMigrations: true,
    });
    wrapped.subscribe({}, () => {});
    fire([{ _id: 'u1', first: 'Ada', last: 'Lovelace' }]);
    await vi.waitFor(() => expect(updateOne).toHaveBeenCalledTimes(1));
  });

  it('passes an un-migratable document to onError rather than throwing', async () => {
    const { col, fire } = subscribeStub([]);
    const wrapped = applySchema(col, {
      schema: { parse: () => { throw new Error('bad shape'); } },
      validateOnRead: true,
      syncSchema: { version: 2 },
      migrateDocument: migrate,
    });
    const errors: unknown[] = [];
    wrapped.subscribe({}, () => {}, (e) => errors.push(e));
    fire([{ _id: 'u1', first: 'Ada' }]);
    expect(errors).toHaveLength(1);
  });

  it('passes documents through untouched when no read transform applies', () => {
    // Only `_v` stamping is active here — reads need no transform, so the
    // subscriber sees the engine's own document objects, not copies.
    const { col, fire } = subscribeStub([]);
    const wrapped = applySchema(col, { syncSchema: { version: 2 } });
    const doc: UserDoc = { _id: 'u1', fullName: 'Ada' };
    const received: UserDoc[][] = [];
    wrapped.subscribe({}, (docs) => received.push(docs));
    fire([doc]);
    expect(received[0][0]).toBe(doc);
  });
});

// ---------------------------------------------------------------------------
// syncSchema validation
// ---------------------------------------------------------------------------

describe('syncSchema validation', () => {
  it('rejects renames without a version', () => {
    // The migration step only runs for documents below `version`; with no
    // version the rename never fires, and `required` then quarantines every
    // document it was meant to fix.
    expect(() =>
      applySchema(stub([]), { syncSchema: { required: ['fullName'], renames: { name: 'fullName' } } }),
    ).toThrow('require syncSchema.version >= 1');
  });

  it('rejects defaults without a version', () => {
    expect(() =>
      applySchema(stub([]), { syncSchema: { defaults: { age: 0 } } }),
    ).toThrow('require syncSchema.version >= 1');
  });

  it('accepts renames when a version is declared', () => {
    expect(() =>
      applySchema(stub([]), { syncSchema: { version: 2, renames: { name: 'fullName' } } }),
    ).not.toThrow();
  });
});

// ---------------------------------------------------------------------------
// Version drift across a heterogeneous fleet.
//
// The scenario these guard: client A is offline for three weeks on v1 while
// client B ships v2 and adds a field. When A reconnects it must (a) still be
// able to *read* B's newer documents, and (b) not destroy B's field just
// because A's build has never heard of it. Under whole-document LWW, a replica
// that silently narrows a document it does not understand does not merely fail
// to see a field — it deletes that field for everyone.
// ---------------------------------------------------------------------------

interface DriftDoc extends Document {
  name?: string;
  /** Introduced by v2. A v1 build has no idea this exists. */
  fullName?: string;
}

/** The v1 build's schema — Zod-like: validates known keys, strips unknown ones. */
const SchemaV1 = {
  parse(doc: unknown): DriftDoc {
    const d = doc as Record<string, unknown>;
    if (typeof d.name !== 'string') throw new Error('name required');
    const out: Record<string, unknown> = { name: d.name };
    if (d._id !== undefined) out._id = d._id;
    if (d._v !== undefined) out._v = d._v;
    return out as DriftDoc;
  },
};

describe('version drift: an old client must not narrow a newer document', () => {
  it('validateOnRead preserves a field the local schema does not model', async () => {
    const col = applySchema(stub([{ _id: 'u1', _v: 2, name: 'Ada', fullName: 'Ada Lovelace' }]), {
      schema: SchemaV1 as never,
      syncSchema: { version: 1 },
      validateOnRead: true,
    });
    const [doc] = await col.find();
    // Zod's parse() would have stripped fullName; the read must restore it, or a
    // read-modify-write in app code writes the truncated doc back.
    expect(doc.fullName).toBe('Ada Lovelace');
    expect(doc.name).toBe('Ada');
  });

  it('still allows a local strict schema to intentionally strip unknown fields', async () => {
    const col = applySchema(stub([{ _id: 'u1', name: 'Ada', injected: 'drop me' } as DriftDoc]), {
      schema: SchemaV1 as never,
      validateOnRead: true,
    });
    const [doc] = await col.find();
    expect(doc).not.toHaveProperty('injected');
  });

  it('persist-on-read write-back does not $unset a newer peer field', async () => {
    const updateOne = vi.fn(async () => true);
    const base = stub([{ _id: 'u1', name: 'Ada', fullName: 'Ada Lovelace' } as DriftDoc]);
    const col = applySchema({ ...base, updateOne } as unknown as Collection<DriftDoc>, {
      syncSchema: { version: 1 },
      // A v1 author's migration. They cannot possibly mention fullName — v2
      // has not been written yet.
      migrateDocument: (d) => ({ _id: d._id, name: d.name ?? '' }) as DriftDoc,
      persistMigrations: true,
    });
    const [doc] = await col.find();
    const [, update] = updateOne.mock.calls[0] as unknown as [unknown, Record<string, unknown>];
    expect(update.$unset).toBeUndefined(); // the deletion that would replicate
    expect(doc.fullName).toBe('Ada Lovelace'); // and it survives the read, too
  });

  it('downgradeDocument projects a newer document into the shape this build reads', async () => {
    const col = applySchema(stub([{ _id: 'u1', _v: 2, fullName: 'Ada Lovelace' } as DriftDoc]), {
      syncSchema: { version: 1 },
      downgradeDocument: (d) => ({ ...d, name: d.fullName }),
    });
    const [doc] = await col.find();
    expect(doc.name).toBe('Ada Lovelace'); // v1 code can read it
    expect(doc._v).toBe(2); // still advertises the shape it really is
  });

  it('forces a downcast to retain the stored identity and higher version', async () => {
    const col = applySchema(stub([{ _id: 'u1', _v: 2, fullName: 'Ada Lovelace' } as DriftDoc]), {
      syncSchema: { version: 1 },
      downgradeDocument: () => ({ _id: 'wrong', _v: 1, name: 'Ada' } as DriftDoc),
    });
    const [doc] = await col.find();
    expect(doc._id).toBe('u1');
    expect(doc._v).toBe(2);
  });

  it('guards updates and deletes so an old client cannot mutate a future document', async () => {
    const updateOne = vi.fn(async () => false);
    const deleteOne = vi.fn(async () => false);
    const base = { ...stub([]), updateOne, deleteOne } as Collection<DriftDoc>;
    const col = applySchema(base, { syncSchema: { version: 1 } });
    await col.updateOne({ _id: 'u1' }, { $set: { name: 'old edit' } });
    await col.deleteOne({ _id: 'u1' });
    expect(updateOne.mock.calls[0]?.[0]).toMatchObject({ $and: expect.any(Array) });
    expect(deleteOne.mock.calls[0]?.[0]).toMatchObject({ $and: expect.any(Array) });
    expect(JSON.stringify(updateOne.mock.calls[0]?.[0])).toContain('$lte');
  });


  it('allows remote replication bulk writes on versioned collections', async () => {
    const replaceManyWithIds = vi.fn(async () => ['u1']);
    const deleteManyWithIds = vi.fn(async () => 1);
    const base = { ...stub([]), replaceManyWithIds, deleteManyWithIds } as Collection<DriftDoc>;
    const col = applySchema(base, { syncSchema: { version: 1 } });
    await col.replaceManyWithIds([{ _id: 'u1', _v: 2, name: 'Ada' } as DriftDoc], 'remote');
    await col.deleteManyWithIds(['u1'], 'remote');
    expect(replaceManyWithIds).toHaveBeenCalledOnce();
    expect(deleteManyWithIds).toHaveBeenCalledOnce();
  });

  it('rejects direct updates to engine-owned metadata', async () => {
    const updateOne = vi.fn(async () => true);
    const col = applySchema({ ...stub([]), updateOne } as Collection<DriftDoc>, {
      syncSchema: { version: 1 },
    });
    await expect(
      col.updateOne({ _id: 'u1' }, { $set: { _v: 2 } }),
    ).rejects.toThrow(/engine-owned field '_v'/);
    expect(updateOne).not.toHaveBeenCalled();
  });

  it('normalizes shape-preserving aggregate and live-aggregate results', async () => {
    const future = { _id: 'u1', _v: 2, fullName: 'Ada Lovelace' } as DriftDoc;
    const aggregate = vi.fn(async () => [future]);
    const subscribeAggregate = vi.fn((_p, cb: (docs: DriftDoc[]) => void) => {
      cb([future]);
      return () => {};
    });
    const base = { ...stub([]), aggregate, subscribeAggregate } as Collection<DriftDoc>;
    const col = applySchema(base, {
      syncSchema: { version: 1 },
      downgradeDocument: (d) => ({ name: d.fullName } as DriftDoc),
    });
    const [row] = await col.aggregate([{ $match: {} }]);
    expect(row).toMatchObject({ _id: 'u1', _v: 2, name: 'Ada Lovelace' });
    let live: DriftDoc[] = [];
    col.subscribeAggregate([{ $limit: 1 }], (docs) => { live = docs; });
    expect(live[0]).toMatchObject({ _v: 2, name: 'Ada Lovelace' });
  });

  it('does not normalize synthetic group/project aggregate rows', async () => {
    const aggregate = vi.fn(async () => [{ _id: 'group', count: 2 } as DriftDoc]);
    const base = { ...stub([]), aggregate } as Collection<DriftDoc>;
    const down = vi.fn((d: Readonly<DriftDoc>) => ({ ...d, name: 'wrong' }));
    const col = applySchema(base, { syncSchema: { version: 1 }, downgradeDocument: down });
    const rows = await col.aggregate([{ $group: { _id: null, count: { $sum: 1 } } } as never]);
    expect(down).not.toHaveBeenCalled();
    expect(rows[0]).not.toHaveProperty('name');
  });

  it('a downcast projection is never written back, even with persistMigrations', async () => {
    const updateOne = vi.fn(async () => true);
    const base = stub([{ _id: 'u1', _v: 2, fullName: 'Ada Lovelace' } as DriftDoc]);
    const col = applySchema({ ...base, updateOne } as unknown as Collection<DriftDoc>, {
      syncSchema: { version: 1 },
      downgradeDocument: (d) => ({ _id: d._id, _v: d._v, name: d.fullName }) as DriftDoc,
      persistMigrations: true,
      allowFieldRemoval: true, // even at its most destructive setting
    });
    await col.find();
    // Persisting the projection would overwrite a v2 document with v1's lossy
    // view of it and push that outward under LWW.
    expect(updateOne).not.toHaveBeenCalled();
  });

  it('downgradeDocument requires syncSchema.version', () => {
    expect(() => applySchema(stub([]), { downgradeDocument: (d) => d })).toThrow(
      'requires syncSchema.version',
    );
  });

  it('leaves an at-version document untouched (neither hook fires)', async () => {
    const up = vi.fn((d: DriftDoc) => d);
    const down = vi.fn((d: DriftDoc) => d);
    const col = applySchema(stub([{ _id: 'u1', _v: 1, name: 'Ada' }]), {
      syncSchema: { version: 1 },
      migrateDocument: up,
      downgradeDocument: down,
    });
    await col.find();
    expect(up).not.toHaveBeenCalled();
    expect(down).not.toHaveBeenCalled();
  });
});
