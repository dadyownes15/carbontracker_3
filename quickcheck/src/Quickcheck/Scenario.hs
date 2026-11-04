{-# LANGUAGE OverloadedStrings #-}
-- | A generated scenario (providers, world, program), its catalog, its
-- validity check and its shrinks. `toCase` turns it into replay input.
module Quickcheck.Scenario
  ( Scenario (..), toCase
  , powerSources, intensitySources, providerOf, resolve, selectionOf
  , validOps, shrinkScenario
  ) where

import qualified Data.Map.Strict as M
import qualified Data.Set as S
import Quickcheck.Json
import Quickcheck.Program

data Scenario = Scenario
  { scPue       :: Double
  , scInterval  :: Us
  , scTimeout   :: Us
  , scProviders :: [Provider]
  , scPairs     :: M.Map SourceId SourceId
  , scTruth     :: M.Map SourceId [(Us, Double)]
  , scPlan      :: Plan
  } deriving (Eq, Show)

toCase :: Scenario -> Case
toCase s = Case
  { casePue = scPue s, caseReadoutInterval = scInterval s, caseTimeout = scTimeout s
  , caseProviders = scProviders s, casePairs = scPairs s, caseTruth = scTruth s
  , caseProgram = scheduled (scPlan s) }

powerSources :: [Provider] -> [(SourceId, Domain)]
powerSources ps =
  [(sourceId (provId p) slot, d) | p <- ps, PowerSource slot d _ <- provSources p]

intensitySources :: [Provider] -> [SourceId]
intensitySources ps =
  [sourceId (provId p) (srcSlot s) | p <- ps, s@IntensitySource{} <- provSources p]

providerOf :: [Provider] -> SourceId -> Maybe Provider
providerOf ps sid =
  case [p | p <- ps, s <- provSources p, sourceId (provId p) (srcSlot s) == sid] of
    (p : _) -> Just p
    [] -> Nothing

-- | The power sources a selection matches on this catalog.
resolve :: [Provider] -> Selection -> S.Set SourceId
resolve ps = \case
  SelAll -> S.fromList (map fst (powerSources ps))
  SelDomains ds -> S.fromList [sid | (sid, d) <- powerSources ps, d `elem` ds]
  SelSources ss -> S.fromList [sid | (sid, _) <- powerSources ps, sid `elem` ss]

-- | Selected sources per span in a case; the root selects all.
selectionOf :: Case -> M.Map SpanId (S.Set SourceId)
selectionOf c = M.insert rootSpan (resolve ps SelAll) $
  M.fromList [(s, resolve ps sel) | Op _ (OpStart s _ sel) <- caseProgram c]
  where ps = caseProviders c

-- | What a realistic caller does: ops in time order, `finish` last and once,
-- each span started once and stopped at most once after its start, marks
-- inside the span, children inside their parents.
validOps :: [Op] -> Either String ()
validOps ops = do
  check (and (zipWith (<=) ts (drop 1 ts))) "ops out of time order"
  check (case reverse ops of (Op _ OpFinish : _) -> True; _ -> False) "finish is not last"
  check (length [() | Op _ OpFinish <- ops] == 1) "more than one finish"
  walk S.empty S.empty M.empty (zip [0 :: Int ..] ops)
  where
    ts = map opAt ops
    check b msg = if b then Right () else Left msg
    walk _ _ _ [] = Right ()
    walk open closed parents ((i, Op _ b) : rest) = case b of
      OpStart s p _ -> do
        check (not (S.member s open || S.member s closed)) ("span started twice at op " ++ show i)
        check (maybe True (`S.member` open) p) ("parent not open at op " ++ show i)
        walk (S.insert s open) closed (M.insert s p parents) rest
      OpMark s -> check (S.member s open) ("mark outside span at op " ++ show i)
        >> walk open closed parents rest
      OpStop s -> do
        check (S.member s open) ("stop without open start at op " ++ show i)
        check (null [c | (c, Just p) <- M.toList parents, p == s, S.member c open])
          ("parent stopped before its child at op " ++ show i)
        walk (S.delete s open) (S.insert s closed) parents rest
      OpFinish -> walk open closed parents rest

-- | Shrinks keep the program valid: whole spans go, marks go, waits halve,
-- threads run in sequence, failures become plain reads, curves go flat.
shrinkScenario :: Scenario -> [Scenario]
shrinkScenario s =
  [s { scPlan = p } | p <- shrinkPlan (scPlan s)]
  ++ [s { scProviders = ps } | ps <- readShrinks (scProviders s)]
  ++ [s { scTruth = t } | t <- truthShrinks (scTruth s)]

readShrinks :: [Provider] -> [[Provider]]
readShrinks ps =
  [ [if provId q == provId p then p { provReads = plain (provReads p) } else q | q <- ps]
  | p <- ps, plain (provReads p) /= provReads p ]
  where
    plain = map $ \case
      ReadOk l -> ReadOk l
      _ -> ReadOk 1000

truthShrinks :: M.Map SourceId [(Us, Double)] -> [M.Map SourceId [(Us, Double)]]
truthShrinks t =
  [M.insert k (take 1 c) t | (k, c) <- M.toList t, length c > 1]

