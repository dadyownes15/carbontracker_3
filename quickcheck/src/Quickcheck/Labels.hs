-- | Coverage labels. Input labels are read off the case; trace labels need
-- the engine's trace. Each property names the label that makes it non-vacuous.
module Quickcheck.Labels
  ( Label (..), labelName, inputLabels, traceLabels, jaggedCuts
  ) where

import qualified Data.Map.Strict as M
import qualified Data.Set as S
import Data.List (nub)
import Quickcheck.Json
import Quickcheck.Scenario

data Label
  = Timeout | LateReturn | Panic | NeverDone | QueuedRelied | Row4Relied
  | JaggedCuts | NestedSpans | FinishOnly | MultipleIntensity | UnservedSelection
  | ManySources | SlowPower | SharedGrid | ManySpans | HundredsOfSpans
  | EightGpus | LongEpoch | LongSession
  deriving (Eq, Show, Enum, Bounded)

labelName :: Label -> String
labelName = \case
  Timeout -> "timeout"
  LateReturn -> "late return"
  Panic -> "panic"
  NeverDone -> "Never provider done"
  QueuedRelied -> "queued request relied on"
  Row4Relied -> "row-4 reliance"
  JaggedCuts -> "jagged cuts"
  NestedSpans -> "nested spans"
  FinishOnly -> "finish-only session"
  MultipleIntensity -> "multiple intensity sources"
  UnservedSelection -> "span selecting nothing served"
  ManySources -> "10 or more sources"
  SlowPower -> "a power provider slower than the interval"
  SharedGrid -> "a grid shared by 3 or more sources"
  ManySpans -> "50 or more spans"
  HundredsOfSpans -> "200 or more spans"
  EightGpus -> "8 GPUs busy at once"
  LongEpoch -> "a span with 100 or more children"
  LongSession -> "a session longer than the grid's minute"

allReads :: Case -> [ReadBehaviour]
allReads c = concatMap provReads (caseProviders c)

inputLabels :: Case -> [(Label, Bool)]
inputLabels c =
  [ (Timeout, any timesOut (allReads c))
  , (LateReturn, any late (allReads c))
  , (Panic, any isPanic (allReads c))
  , (JaggedCuts, jaggedCuts c)
  , (NestedSpans, not (null [() | Op _ (OpStart _ (Just _) _) <- caseProgram c]))
  , (FinishOnly, length (caseProgram c) == 1)
  , (MultipleIntensity, length (intensitySources (caseProviders c)) >= 2)
  , (UnservedSelection, any S.null (M.elems (selectionOf c)))
  , (ManySources, length [() | p <- ps, _ <- provSources p] >= 10)
  , (SlowPower, any slower ps)
  , (SharedGrid, any (>= 3) (M.elems (M.fromListWith (+) [(i, 1 :: Int) | i <- M.elems (casePairs c)])))
  , (ManySpans, spans >= 50)
  , (HundredsOfSpans, spans >= 200)
  , (EightGpus, busyGpus c >= 8)
  , (LongEpoch, any (>= 100) (M.elems (M.fromListWith (+) [(p, 1 :: Int) | Op _ (OpStart _ (Just p) _) <- caseProgram c])))
  , (LongSession, maybe False ((> 60000000) . opAt) (lastOp (caseProgram c)))
  ]
  where
    ps = caseProviders c
    spans = length [() | Op _ (OpStart {}) <- caseProgram c]
    slower p = any isPower (provSources p) && maybe True (> caseReadoutInterval c) (provCooldown p)
    isPower = \case PowerSource {} -> True; _ -> False
    lastOp xs = if null xs then Nothing else Just (last xs)
    t = caseTimeout c
    timesOut = \case ReadHang -> True; r -> late r
    late = \case ReadOk l -> l > t; ReadEmpty l -> l > t; _ -> False
    isPanic = \case ReadPanic _ -> True; _ -> False

-- | The most GPUs at one instant that each have a span of their own open.
busyGpus :: Case -> Int
busyGpus c = maximum (0 : map busy (scanl step S.empty (caseProgram c)))
  where
    sel = selectionOf c
    gpus = S.fromList [s | (s, Gpu) <- powerSources (caseProviders c)]
    own s = let x = M.findWithDefault S.empty s sel in
      if S.size x == 1 && x `S.isSubsetOf` gpus then x else S.empty
    busy = S.size . S.unions . map own . S.toList
    step open (Op _ b) = case b of
      OpStart s _ _ -> S.insert s open
      OpStop s -> S.delete s open
      _ -> open

-- | Two spans on disjoint sources, overlapping in time, each cut at least
-- twice, and no two ops of the program at the same instant.
jaggedCuts :: Case -> Bool
jaggedCuts c = distinct && or
  [ True
  | (a, (sa, ta)) <- spans, (b, (sb, tb)) <- spans, a < b
  , S.disjoint sa sb, not (S.null sa), not (S.null sb)
  , length ta >= 2, length tb >= 2
  , minimum ta < maximum tb, minimum tb < maximum ta ]
  where
    ops = [o | o <- caseProgram c, opBody o /= OpFinish]
    distinct = length (nub (map opAt ops)) == length ops
    sel = selectionOf c
    spans = M.toList $ M.fromListWith (\(s, x) (_, y) -> (s, x ++ y))
      [ (s, (M.findWithDefault S.empty s sel, [t])) | Op t b <- ops, Just s <- [spanOf b] ]
    spanOf = \case
      OpStart s _ _ -> Just s
      OpMark s -> Just s
      OpStop s -> Just s
      OpFinish -> Nothing

traceLabels :: Case -> Trace -> [(Label, Bool)]
traceLabels c tr =
  [ (NeverDone, or [ cutAt cut > firstRead p
                   | cut <- traceCuts tr, (src, Nothing) <- M.toList (cutRelied cut)
                   , p <- neverIds, src `elem` sourcesOf p ])
  , (QueuedRelied, or [ reqQueued r | r <- relied ])
  , (Row4Relied, or [ reqAnswer r `elem` [Just AnsSkipped, Just AnsFailed] && reqSent r < cutAt cut
                    | cut <- traceCuts tr, Just i <- M.elems (cutRelied cut)
                    , Just r <- [M.lookup i byId] ])
  ]
  where
    neverIds = [provId p | p <- caseProviders c, provCooldown p == Nothing]
    sourcesOf p = [sourceId p (srcSlot s) | q <- caseProviders c, provId q == p, s <- provSources q]
    firstRead p = minimum (maxBound : [a | r <- traceRequests tr, reqProvider r == p
                                         , reqAnswer r == Just AnsRead, Just a <- [reqAnswered r]])
    byId = M.fromList [(reqId r, r) | r <- traceRequests tr]
    relied = [r | cut <- traceCuts tr, Just i <- M.elems (cutRelied cut), Just r <- [M.lookup i byId]]
