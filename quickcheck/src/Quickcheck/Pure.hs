-- | Properties checked exactly on replay-pure. Each case is a generated
-- program and world, turned into session events by 'toPure'; the expected
-- values come from the program and the world only.
module Quickcheck.Pure
  ( pureTests, PureTest (..), runPureTest
  ) where

import qualified Data.Map.Strict as M
import qualified Data.Set as S
import qualified Data.Text as T
import Data.Aeson (encode)
import qualified Data.ByteString.Lazy as BL
import Data.List (sortOn)
import Data.Maybe (fromMaybe, listToMaybe)
import System.Directory (createDirectoryIfMissing)
import System.Environment (lookupEnv)
import System.FilePath ((</>))
import Test.QuickCheck
import Quickcheck.Gen
import Quickcheck.Json
import Quickcheck.Reads
import Quickcheck.Runner
import Quickcheck.Scenario

data PureTest = PureTest
  { ptName :: String
  , ptGen  :: Gen Scenario
  , ptProp :: Case -> Built -> PureTrace -> Property
  }

pureTests :: [PureTest]
pureTests =
  [ PureTest "the session drains; every closed span settles once, no later than its parent"
      genScenario settles
  , PureTest "each source's segments are accounted once, in order, tiling the session"
      genScenario tiles
  , PureTest "a span is billed only for sources it selected"
      genScenario selectedOnly
  , PureTest "constant world, Queue: each source is billed PUE x watts x session length"
      genConstantScenario conserves
  , PureTest "constant world: emissions are energy x intensity, absent with no intensity"
      genConstantScenario emits
  , PureTest "a span counts the samples inside it, and warns of thin coverage below two"
      genScenario coverage
  ]

-- | Runs one test against replay-pure. A failing case's input is written to
-- $QUICKCHECK_FAILURES (default failures/) for replay by hand.
-- AC/TST-35
runPureTest :: FilePath -> PureTest -> Property
runPureTest bin t = checkCoverage $ forAllShrink (ptGen t) shrinkScenario $ \s ->
  let c = toCase s
      b = toPure c
  in ioProperty $ do
    r <- runPure bin (builtCase b)
    pure $ whenFail (save (builtCase b)) $ covered b $ cover 15 (disjointChild c) "a child selects a source its parent did not" $ case r of
      Left e -> counterexample e False
      Right tr -> ptProp t c b tr
  where
    save pc = do
      dir <- fromMaybe "failures" <$> lookupEnv "QUICKCHECK_FAILURES"
      createDirectoryIfMissing True dir
      let path = dir </> (map slug (ptName t) ++ ".json")
      BL.writeFile path (encode pc)
      putStrLn ("input written to " ++ path)
    slug ch = if ch `elem` (['a' .. 'z'] ++ ['A' .. 'Z'] ++ ['0' .. '9']) then ch else '-'

-- | The minimum share of cases that must exercise each kind of reliance and
-- failure, or the run fails as too trivial.
-- AC/TST-33
covered :: Built -> Property -> Property
covered b =
  cover 20 (Queued `elem` rel) "a cut relies on a queued request"
    . cover 20 (AnsweredNow `elem` rel) "a cut relies on an answer with no read"
    . cover 5 (ReadOut `elem` rel) "a cut relies on a read already out"
    . cover 10 (builtTimeouts b > 0) "a request times out"
    . cover 5 (builtPanics b > 0) "a provider panics"
    . cover 30 (builtWatchdog b > 0) "the watchdog reads"
    . tabulate "reliance" (map show rel)
  where rel = builtReliance b

finishOf :: Built -> Us
finishOf b = fromMaybe 0 (builtFinish b)

-- Properties -------------------------------------------------------------

-- AC/TST-16, AC/PROF-32, AC/PROF-33
settles :: Case -> Built -> PureTrace -> Property
settles c _ t = conjoin
  [ counterexample ("refused: " ++ show (ptRefused t)) (null (ptRefused t))
  , counterexample "the session did not drain after finish" (pssDrained (ptSession t))
  , conjoin [ counterexample ("span " ++ show s ++ " settled " ++ show n ++ " times") (n == 1)
            | s <- closed, let n = length [() | x <- ptSettled t, settledSpan x == s] ]
  , conjoin [ counterexample ("span " ++ show s ++ " settled after its parent " ++ show p) (sAt <= pAt)
            | (s, Just p) <- parents, s `elem` closed, p `elem` closed
            , Just sAt <- [settledAt' s], Just pAt <- [settledAt' p] ] ]
  where
    closed = [s | Op _ (OpStop s) <- caseProgram c]
    parents = [(s, p) | Op _ (OpStart s p _) <- caseProgram c]
    settledAt' s = case [settledAt x | x <- ptSettled t, settledSpan x == s] of
      (a : _) -> Just a
      [] -> Nothing

-- AC/TST-10, AC/TST-14
tiles :: Case -> Built -> PureTrace -> Property
tiles c b t = conjoin
  [ counterexample (T.unpack src ++ " in accounting order: " ++ show spans) $
      conjoin [ counterexample "not accounted in order" (map fst spans == sortOn id (map fst spans))
              , counterexample "a gap, an overlap or a repeat" (and (zipWith (\(_, to) (from, _) -> to == from) spans (drop 1 spans)))
              , counterexample "does not start at the opening" (take 1 (map fst spans) == [0])
              , counterexample "does not end at finish" (lastTo spans == Just (finishOf b)) ]
  | (src, _) <- powerSources (caseProviders c)
  , let spans = [(psFrom s, psTo s) | s <- ptSegments t, psSource s == src] ]
  where lastTo xs = if null xs then Nothing else Just (snd (last xs))

-- AC/TST-28
selectedOnly :: Case -> Built -> PureTrace -> Property
selectedOnly c _ t = conjoin
  [ counterexample ("span " ++ show k ++ " billed for " ++ T.unpack (psSource s) ++ " it did not select")
      (k == rootSpan || S.member (psSource s) (M.findWithDefault S.empty k sel))
  | s <- ptSegments t, k <- psClaimants s ]
  where sel = selectionOf c

-- | Sources that landed at least one sample.
sampled :: Built -> S.Set SourceId
sampled b = S.fromList [src | PureEvent _ (PLand _ ss) <- pcEvents (builtCase b), (src, _ : _) <- M.toList ss]

-- AC/TST-15, AC/PROF-31, AC/MEAS-92
-- | Exact under Queue unless the provider hangs or panics: a failed read
-- leaves its request open and the provider is read again (08, when a read
-- fails). A hang outlasts the retry and a panic ends it; then a segment may
-- be billed from what the store held, so never more than the truth.
conserves :: Case -> Built -> PureTrace -> Property
conserves c b t = cover 40 (not (null exact)) "a Queue source with no hang or panic is checked exactly" $ conjoin
  [ counterexample (T.unpack src ++ ": billed " ++ show got ++ " J, expected " ++ (if healthy then "" else "at most ") ++ show want)
      (if healthy then near got want else got <= want * (1 + 1e-9))
  -- Exact only under Queue: under Skip a segment may be accounted before its
  -- samples land (the trade-off in 08, held values).
  | p <- caseProviders c, provPolicy p == Queue, PowerSource slot _ kind <- provSources p, kind /= Total
  , let src = sourceId (provId p) slot
  , S.member src (sampled b)
  , let healthy = all ok (provReads p)
        watts = constant src
        want = casePue c * watts * fromIntegral (finishOf b) * 1e-6
        got = sum [psEnergy s | s <- ptSegments t, psSource s == src] ]
  where
    constant src = maybe 0 snd (listToMaybe (M.findWithDefault [] src (caseTruth c)))
    ok = \case ReadHang -> False; ReadPanic _ -> False; _ -> True
    exact = [ () | p <- caseProviders c, provPolicy p == Queue, all ok (provReads p)
                 , PowerSource slot _ kind <- provSources p, kind /= Total
                 , S.member (sourceId (provId p) slot) (sampled b) ]

-- AC/TST-29, AC/TST-9
emits :: Case -> Built -> PureTrace -> Property
emits c b t = conjoin
  [ case (M.lookup (psSource s) (casePairs c), psEmissions s) of
      (Nothing, e) -> counterexample (show s ++ ": no intensity, yet emissions") (psEnergy s == 0 || e == Nothing)
      (Just i, Just e) | S.member i (sampled b) ->
        counterexample (show s ++ ": emissions " ++ show e) (near e (psEnergy s / 3.6e6 * constant i))
      _ -> property True
  | s <- ptSegments t ]
  where constant src = maybe 0 snd (listToMaybe (M.findWithDefault [] src (caseTruth c)))

near :: Double -> Double -> Bool
near x y = abs (x - y) <= 1e-9 * max 1 (abs y)

-- | Some span selects a source its parent does not.
disjointChild :: Case -> Bool
disjointChild c = or
  [ not (S.null (S.difference (sel s) (sel p)))
  | Op _ (OpStart s (Just p) _) <- caseProgram c ]
  where
    sels = selectionOf c
    sel k = M.findWithDefault S.empty k sels

-- | For each settled span and each power source it selected: the samples it
-- reports are those stamped inside @[start, stop)@ that had landed when the
-- segment holding them was accounted, and it warns of thin coverage exactly
-- when one of them has fewer than two. Short spans on both sides of two are
-- required, so blinks that straddle a watchdog read are exercised.
-- AC/TST-30, AC/PROF-47
coverage :: Case -> Built -> PureTrace -> Property
coverage c b t =
  cover 5 (or [n < 2 | (_, n) <- short]) "a short span sees fewer than two samples"
    $ cover 5 (or [n >= 2 | (_, n) <- short]) "a short span sees two or more samples"
    $ conjoin
        [ conjoin
            [ counterexample ("span " ++ show (srSpan r) ++ " reports " ++ show n ++ " samples of " ++ T.unpack src ++ ", expected " ++ show want)
                (n == want)
            | (src, n) <- M.toList (srSamples r)
            , let want = expected (srSpan r) src ]
          .&&. counterexample ("span " ++ show (srSpan r) ++ " warns " ++ show (srWarnings r) ++ " with samples " ++ show (srSamples r))
                 ((T.pack "thin_coverage" `elem` srWarnings r) == any (< 2) (M.elems (srSamples r)))
        | r <- ptSpans t, M.member (srSpan r) bounds ]
  where
    bounds = M.intersectionWith (,)
      (M.fromList [(s, at) | Op at (OpStart s _ _) <- caseProgram c])
      (M.fromList [(s, at) | Op at (OpStop s) <- caseProgram c])
    -- Each sample: its source, its stamp, and when its readout landed.
    landed = [ (src, stamp, at) | PureEvent at (PLand _ ss) <- pcEvents (builtCase b)
             , (src, points) <- M.toList ss, (stamp, _) <- points ]
    accountedAt src stamp = case [psAccounted g | g <- ptSegments t, psSource g == src, psFrom g <= stamp, stamp < psTo g] of
      (x : _) -> Just x
      [] -> Nothing
    expected s src = case M.lookup s bounds of
      Nothing -> 0
      Just (from, to) -> length
        [ () | (src', stamp, at) <- landed, src' == src, from <= stamp, stamp < to
        , maybe False (at <=) (accountedAt src stamp) ]
    short =
      [ (s, n) | r <- ptSpans t, let s = srSpan r, Just (from, to) <- [M.lookup s bounds]
      , to - from < caseReadoutInterval c, n <- M.elems (srSamples r) ]
