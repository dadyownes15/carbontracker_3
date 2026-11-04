-- | The properties. Self-tests check the generators, interpreter and oracle
-- and always run. Trace properties run each case through replay; the oracle
-- is the world itself, never a model of the engine.
module Quickcheck.Properties
  ( selfTests, traceTests, TraceTest (..), runTraceTest
  ) where

import qualified Data.Map.Strict as M
import qualified Data.Set as S
import Data.Aeson (encode)
import qualified Data.ByteString.Lazy as BL
import Data.List (sortOn)
import Data.Maybe (isJust, fromMaybe)
import System.Directory (createDirectoryIfMissing)
import System.Environment (lookupEnv)
import System.FilePath ((</>))
import Test.QuickCheck
import Quickcheck.Gen
import Quickcheck.Json
import Quickcheck.Labels
import Quickcheck.Runner
import Quickcheck.Scenario
import Quickcheck.World

-- Self-tests ------------------------------------------------------------

coverInputs :: Case -> Property -> Property
coverInputs c p = foldr (\(l, b) q -> cover (pct l) b (labelName l) q) p (inputLabels c)
  where
    pct = \case
      FinishOnly -> 2
      UnservedSelection -> 2
      MultipleIntensity -> 20
      ManySources -> 10
      SlowPower -> 10
      SharedGrid -> 10
      ManySpans -> 10
      HundredsOfSpans -> 2
      EightGpus -> 1
      LongEpoch -> 2
      LongSession -> 5
      _ -> 3

selfTests :: [(String, Property)]
selfTests =
  [ ( "generated programs are valid; generators reach every input label"
    , checkCoverage $ forAll genScenario $ \s ->
        let c = toCase s in coverInputs c (validProp c) )
  , ( "shrinks keep programs valid"
    , forAll genScenario $ \s ->
        conjoin [validProp (toCase s') | s' <- take 40 (shrinkScenario s)] )
  , ( "jagged threads never cut at the same instant (label: jagged cuts)"
    , checkCoverage $ forAll genJaggedScenario $ \s ->
        let c = toCase s in cover 80 (jaggedCuts c) "jagged cuts" (validProp c) )
  , ( "clean scenarios: instant reads, Queue, constant truth"
    , forAll genCleanScenario $ \s ->
        let c = toCase s in
        validProp c .&&. all (all (== ReadOk 0) . provReads) (caseProviders c)
        .&&. all ((== Queue) . provPolicy) (caseProviders c)
        .&&. all isConstant (M.elems (caseTruth c)) )
  , ( "oracle: integrals add over adjacent intervals"
    , forAll genScenario $ \s -> forAll (choose (0, 10000000)) $ \a ->
        forAll (choose (a, 10000000)) $ \b -> forAll (choose (b, 10000000)) $ \x ->
        conjoin [ close (integral cv a b + integral cv b x) (integral cv a x)
                | cv <- M.elems (scTruth s) ] )
  , ( "oracle: a flat curve integrates to value times length"
    , property $ \(Positive v) (NonNegative a) (NonNegative d) ->
        close (energyJ [(0, v)] a (a + d)) (v * fromIntegral d * 1e-6) )
  ]

validProp :: Case -> Property
validProp c = case validOps (caseProgram c) of
  Right () -> property True
  Left e -> counterexample e False

close :: Double -> Double -> Property
close x y = counterexample (show x ++ " /= " ++ show y) $
  abs (x - y) <= 1e-6 * max 1 (abs y)

-- Trace properties ------------------------------------------------------

data TraceTest = TraceTest
  { ttName  :: String
  , ttLabel :: Label
  , ttGen   :: Gen Scenario
  , ttProp  :: Case -> Trace -> Property
  }

traceTests :: [TraceTest]
traceTests =
  [ TraceTest "every closed span settles once, children first; the session ends"
      NestedSpans genScenario settlesOnce
  , TraceTest "each segment is accounted at most once, in order per source"
      JaggedCuts genScenario accountedOnce
  , TraceTest "clean Queue: ledgers per source = PUE x exact integral of truth"
      MultipleIntensity genCleanScenario exactTotals
  , TraceTest "Queue: accounted within max(cooldown, timeout) + timeout of its end"
      QueuedRelied genScenario accountedInBound
  , TraceTest "while a provider stays unready, cuts send it at most one request"
      Row4Relied genScenario oneRequestPerStretch
  , TraceTest "a span is billed only for sources it selected"
      JaggedCuts genScenario billedOnlySelected
  ]

-- | Runs one trace test against the binary. A failing case writes its input
-- to $QUICKCHECK_FAILURES (default failures/) for replay by hand.
runTraceTest :: FilePath -> TraceTest -> Property
runTraceTest bin tt = checkCoverage $ forAllShrink (ttGen tt) shrinkScenario $ \s ->
  let c = toCase s in ioProperty $ do
    r <- runReplay bin c
    pure $ whenFail (save c) $ case r of
      Left e -> counterexample e False
      Right t ->
        let labels = inputLabels c ++ traceLabels c t
            named = fromMaybe False (lookup (ttLabel tt) labels)
        in cover 2 named (labelName (ttLabel tt))
             $ tabulate "labels" [labelName l | (l, True) <- labels]
             $ ttProp tt c t
  where
    save c = do
      dir <- fromMaybe "failures" <$> lookupEnv "QUICKCHECK_FAILURES"
      createDirectoryIfMissing True dir
      let path = dir </> (map slug (ttName tt) ++ ".json")
      BL.writeFile path (encode c)
      putStrLn ("input written to " ++ path)
    slug ch = if ch `elem` (['a' .. 'z'] ++ ['A' .. 'Z'] ++ ['0' .. '9']) then ch else '-'

closedSpans :: Case -> [(SpanId, Maybe SpanId)]
closedSpans c =
  [ (s, p) | Op _ (OpStart s p _) <- caseProgram c
  , s `elem` [x | Op _ (OpStop x) <- caseProgram c] ]

settlesOnce :: Case -> Trace -> Property
settlesOnce c t = conjoin
  [ counterexample ("refused: " ++ show (traceRefused t)) (null (traceRefused t))
  , counterexample "session did not end" (isJust (sessionEnded (traceSession t)))
  , conjoin [ counterexample ("span " ++ show s ++ " settled " ++ show n ++ " times") (n == 1)
            | (s, _) <- closed, let n = length [() | x <- traceSettled t, settledSpan x == s] ]
  , conjoin [ counterexample ("span " ++ show s ++ " settled after its parent " ++ show p) (cs <= ps)
            | (s, Just p) <- closed, Just cs <- [at s], Just ps <- [at p] ]
  ]
  where
    closed = closedSpans c
    at s = lookup s [(settledSpan x, settledAt x) | x <- traceSettled t]

bySource :: Trace -> M.Map SourceId [Segment]
bySource t = M.map (sortOn segFrom) $
  M.fromListWith (++) [(segSource s, [s]) | s <- traceSegments t]

accountedOnce :: Case -> Trace -> Property
accountedOnce _ t = conjoin
  [ counterexample ("source " ++ show src ++ ": " ++ show segs) $
      noDup segs .&&. inOrder (map segAccounted segs)
  | (src, segs) <- M.toList (bySource t) ]
  where
    noDup segs = let ks = map segFrom segs in S.size (S.fromList ks) == length ks
    inOrder xs = case span isJust xs of
      (done, rest) -> all (== Nothing) rest && and (zipWith (<=) done (drop 1 done))

exactTotals :: Case -> Trace -> Property
exactTotals c t = conjoin $
  [ counterexample ("energy of " ++ show src) $ close got want
  | (src, _) <- powerSources (caseProviders c)
  , let segs = M.findWithDefault [] src (bySource t)
        got = sum (map segEnergy segs)
        want = casePue c * energyJ (truth src) 0 end ]
  ++
  [ counterexample ("emissions of " ++ show src) $
      close (sum [fromMaybe 0 (segEmissions g) | g <- segs])
            (sum (map segEnergy segs) / 3.6e6 * valueAt (truth i) 0)
  | (src, _) <- powerSources (caseProviders c)
  , Just i <- [M.lookup src (casePairs c)]
  , let segs = M.findWithDefault [] src (bySource t) ]
  ++
  [ counterexample "session total" $
      close (sessionEnergy (traceSession t)) (sum (map segEnergy (traceSegments t))) ]
  where
    end = opAt (last (caseProgram c))
    truth s = M.findWithDefault [] s (caseTruth c)

accountedInBound :: Case -> Trace -> Property
accountedInBound c t = conjoin
  [ counterexample ("segment " ++ show g ++ " bound " ++ show b) $
      maybe False (\a -> a - segTo g <= b) (segAccounted g)
  | g <- traceSegments t
  , let provs = [p | Just p <- [providerOf ps (segSource g)]]
             ++ [p | Just i <- [M.lookup (segSource g) (casePairs c)], Just p <- [providerOf ps i]]
  , all ((== Queue) . provPolicy) provs
  , Just cds <- [mapM provCooldown provs]
  , let b = maximum (0 : [max cd to + to | cd <- cds]) ]
  where
    ps = caseProviders c
    to = caseTimeout c

-- | A stretch is an interval between consecutive transitions of a provider
-- (a read starting, returning, timing out, its cooldown passing) during which
-- the provider is unready. Cut requests sent inside it, other than one that
-- starts a read, number at most one.
oneRequestPerStretch :: Case -> Trace -> Property
oneRequestPerStretch c t = conjoin
  [ counterexample ("provider " ++ show (provId p) ++ " stretch " ++ show (x, y) ++ ": " ++ show inside)
      (length inside <= 1)
  | p <- caseProviders c
  , let reqs = [r | r <- traceRequests t, reqProvider r == provId p]
        reads = zip [0 :: Int ..] (sortOn reqStarted [r | r <- reqs, isJust (reqStarted r)])
        behaviour k = let rs = provReads p in if k < length rs then rs !! k else last rs
        info = [ (s, reqAnswered r, isPanic (behaviour k)) | (k, r) <- reads, Just s <- [reqStarted r] ]
        cd = provCooldown p
        marks = S.toList . S.fromList $ concat
          [ [s, s + to] ++ maybe [] (\d -> [s + d]) cd ++ maybe [] pure ret | (s, ret, _) <- info ]
        bounds = zip marks (map Just (drop 1 marks) ++ [Nothing])
  , (x, y) <- bounds
  , unready info cd x
  , let inside = [ reqId r | r <- reqs, reqAsker r == AskCut
                 , reqSent r >= x, maybe True (reqSent r <) y
                 , reqStarted r /= Just (reqSent r) ] ]
  where
    to = caseTimeout c
    isPanic = \case ReadPanic _ -> True; _ -> False
    unready info cd x = or
      [ s <= x && (inCooldown || inFlight || retired)
      | (s, ret, panicked) <- info
      , let inCooldown = maybe True (\d -> x < s + d) cd
            inFlight = maybe True (> x) ret
            retired = panicked && maybe False (<= x) ret ]

billedOnlySelected :: Case -> Trace -> Property
billedOnlySelected c t = conjoin
  [ counterexample ("span " ++ show k ++ " billed for " ++ show (segSource g)) $
      maybe False (S.member (segSource g)) (M.lookup k sel)
  | g <- traceSegments t, k <- segClaimants g ]
  where sel = selectionOf c
