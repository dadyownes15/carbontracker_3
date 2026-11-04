{-# LANGUAGE OverloadedStrings #-}
-- | Realistic input: hosts with RAPL-, NVML- and grid-like providers, worlds
-- of piecewise-linear truth, and caller programs shaped like real use.
module Quickcheck.Gen
  ( genScenario, genCleanScenario, genJaggedScenario, genConstantScenario
  , genProviders, genProgram, shapes, Failures (..)
  ) where

import Control.Monad (forM, replicateM)
import qualified Data.Map.Strict as M
import Data.List (sort, nub)
import Test.QuickCheck
import Quickcheck.Json
import Quickcheck.Program
import Quickcheck.Scenario

data Failures = WithFailures | NoFailures deriving (Eq, Show)

ms, sec :: Us
ms = 1000
sec = 1000000

timeoutUs :: Us
timeoutUs = 2 * sec

-- Providers -------------------------------------------------------------

genReads :: Failures -> Gen [ReadBehaviour]
genReads NoFailures = pure [ReadOk 0]
genReads WithFailures = do
  n <- choose (4, 12)
  rs <- replicateM n $ frequency
    [ (12, ReadOk <$> choose (100, 20 * ms))
    , (1, pure ReadHang)
    , (1, ReadOk <$> choose (timeoutUs + 1, timeoutUs + 3 * sec)) -- late return
    , (1, ReadPanic <$> choose (0, 50 * ms))
    , (2, ReadEmpty <$> choose (100, 5 * ms)) ]
  pure (rs ++ [ReadOk (5 * ms)])

plainReads :: Gen [ReadBehaviour]
plainReads = (\l -> [ReadOk l]) <$> choose (100, 20 * ms)

-- | RAPL-like (10 ms), optionally NVML-like (1 s, Queue), a grid intensity
-- provider (1 min, Skip) and optionally a Never provider of static intensity.
-- | A host: a realistic preset now and then, otherwise any mix of providers,
-- growing with the size parameter.
genProviders :: Failures -> Gen [Provider]
genProviders f = frequency [(2, preset f), (5, realHost f), (3, general f)]

-- | The hosts carbontracker runs on, at their real size.
-- AC/TST-32
realHost :: Failures -> Gen [Provider]
realHost f = do
  host <- frequency [(2, intelLaptop), (2, appleSilicon), (5, gpuServer), (2, cloudVm)]
  grid <- gridProviders f
  pure (host ++ grid)
  where
    pol = if f == NoFailures then pure Queue else frequency [(4, pure Queue), (1, pure Skip)]
    reads = if f == NoFailures then genReads NoFailures else frequency [(3, plainReads), (1, genReads WithFailures)]
    -- RAPL: a package and a DRAM energy counter per socket, every 10 ms.
    rapl i sockets = Provider i (Just (10 * ms)) <$> pol <*> reads
      <*> pure (concat [[PowerSource (2 * k) Cpu Total, PowerSource (2 * k + 1) Ram Total] | k <- [0 .. sockets - 1]])
    intelLaptop = (: []) <$> rapl 1 1
    -- IOReport: cpu, gpu and ane energy counters of one Apple Silicon chip.
    appleSilicon = (: []) <$> (Provider 1 (Just (10 * ms)) <$> pol <*> reads
      <*> pure [PowerSource 0 Cpu Total, PowerSource 1 Gpu Total, PowerSource 2 Ane Total])
    -- One to four sockets and the GPU counts machines are sold with, up to
    -- eight; NVML reads power or energy.
    gpuServer = do
      sockets <- frequency [(1, pure 1), (6, pure 2), (1, pure 4)]
      (:) <$> rapl 1 sockets <*> ((: []) <$> nvml)
    nvml = sized $ \n -> do
      gpus <- frequency [(w, pure g) | (w, g) <- [(2, 1), (2, 2), (2, 4), (4, 8)], g <= 1 + n `div` 6]
      energy <- arbitrary
      let (cooldown, kind) = if energy then (sec, Total) else (100 * ms, Instant)
      Provider 2 (Just cooldown) <$> pol <*> reads <*> pure [PowerSource k Gpu kind | k <- [0 .. gpus - 1]]
    -- A cloud VM: no counters for the CPU, only its modelled draw (Never),
    -- and often NVML for the GPUs passed through.
    cloudVm = do
      cpu <- Provider 1 Nothing <$> pol <*> reads <*> pure [PowerSource 0 Cpu Instant]
      withGpus <- frequency [(3, pure True), (1, pure False)]
      gpus <- nvml
      pure ([cpu] ++ [gpus | withGpus])

-- | A live grid read every minute, and sometimes a static fallback.
gridProviders :: Failures -> Gen [Provider]
gridProviders f = do
  let pol = if f == NoFailures then Queue else Skip
      reads = if f == NoFailures then genReads NoFailures else frequency [(3, plainReads), (1, genReads WithFailures)]
  live <- Provider 9 (Just (60 * sec)) pol <$> reads <*> pure [IntensitySource 0]
  withStatic <- arbitrary
  static <- Provider 10 Nothing Queue <$> reads <*> pure [IntensitySource 0]
  pure ([live] ++ [static | withStatic])

-- | Any host: 1 to 12 providers, each serving 1 to 4 sources of one kind,
-- with cooldowns, policies and failures drawn independently. At least one
-- serves power.
general :: Failures -> Gen [Provider]
general f = sized $ \n -> do
  k <- choose (1, max 1 (min 12 (1 + n `div` 8)))
  kinds <- (True :) <$> vectorOf (k - 1) (frequency [(3, pure True), (1, pure False)])
  forM (zip [1 ..] kinds) $ \(i, power) -> do
    m <- choose (1, 4)
    sources <- if power
      then forM [0 .. m - 1] $ \slot -> PowerSource slot <$> elements [Cpu, Gpu, Ram] <*> elements [Instant, Held, Total]
      else pure [IntensitySource slot | slot <- [0 .. m - 1]]
    Provider i <$> cooldownOf <*> policyOf <*> readsOf <*> pure sources
  where
    -- Log-uniform from 0.1 ms to 60 s, sometimes zero, sometimes Never.
    cooldownOf = frequency
      [ (1, pure Nothing), (1, pure (Just 0))
      , (8, (\e -> Just (round (10 ** e :: Double))) <$> choose (2, logBase 10 (60 * fromIntegral sec))) ]
    policyOf = if f == NoFailures then pure Queue else elements [Queue, Skip]
    readsOf = if f == NoFailures then genReads NoFailures else frequency [(2, plainReads), (1, genReads WithFailures)]

-- | A laptop or a GPU box: RAPL-, NVML- and grid-like providers.
preset :: Failures -> Gen [Provider]
preset f = do
  let reads = if f == NoFailures then genReads NoFailures else
        frequency [(2, plainReads), (1, genReads WithFailures)]
      pol = if f == NoFailures then pure Queue else frequency [(4, pure Queue), (1, pure Skip)]
  raplKind <- if f == NoFailures then elements [Instant, Total] else elements [Total, Total, Instant, Held]
  rapl <- Provider 1 (Just (10 * ms)) <$> pol <*> reads
            <*> pure [PowerSource 0 Cpu raplKind, PowerSource 1 Ram raplKind]
  withGpu <- frequency [(3, pure True), (2, pure False)]
  nvml <- Provider 2 (Just sec) Queue <$> reads <*> pure [PowerSource 0 Gpu Instant]
  gridPol <- if f == NoFailures then pure Queue else pure Skip
  grid <- Provider 9 (Just (60 * sec)) gridPol <$> reads <*> pure [IntensitySource 0]
  withNever <- arbitrary
  never <- Provider 10 Nothing <$> pol <*> reads <*> pure [IntensitySource 0]
  pure ([rapl] ++ [nvml | withGpu] ++ [grid] ++ [never | withNever])

genPairs :: [Provider] -> Gen (M.Map SourceId SourceId)
genPairs ps = case intensitySources ps of
  [] -> pure M.empty
  is -> fmap (M.fromList . concat) . forM (map fst (powerSources ps)) $ \sid ->
    frequency [(9, (\i -> [(sid, i)]) <$> elements is), (1, pure [])]

-- World -----------------------------------------------------------------

range :: Domain -> (Double, Double)
range = \case
  Cpu -> (20, 150)
  Ram -> (3, 12)
  Gpu -> (50, 300)
  Ane -> (0, 8)

-- | Piecewise-linear truth over [0, horizon], values in realistic ranges.
genCurve :: (Double, Double) -> Us -> Gen [(Us, Double)]
genCurve (lo, hi) horizon = do
  n <- choose (1, 6)
  ts <- nub . sort <$> replicateM n (choose (0, max 1 horizon))
  forM ts $ \t -> (,) t . roundTo <$> choose (lo, hi)
  where roundTo x = fromIntegral (round (x * 100) :: Int) / 100

constCurve :: (Double, Double) -> Gen [(Us, Double)]
constCurve r = (\v -> [(0, v)]) <$> choose r

genTruth :: Bool -> [Provider] -> Us -> Gen (M.Map SourceId [(Us, Double)])
genTruth constant ps horizon = do
  let curve r = if constant then constCurve r else genCurve r horizon
  pw <- forM (powerSources ps) $ \(sid, d) -> (,) sid <$> curve (range d)
  is <- forM (intensitySources ps) $ \sid -> (,) sid <$> curve (50, 500)
  pure (M.fromList (pw ++ is))

-- Programs --------------------------------------------------------------
-- Each shape is a plan of real use: what a training loop, a server or a
-- notebook does, written as nested spans.

-- | An epoch over all sources, its steps on the GPU: a few steps, or a
-- long epoch of up to twice the size.
training :: Gen Plan
training = do
  n <- stepCount
  base <- stepTime
  marks <- arbitrary
  steps <- replicateM n $ do
    d <- nearly base
    gap <- choose (0, 5 * ms)
    pure ([Over (SelDomains [Gpu]) [Idle d]] ++ [MarkHere | marks] ++ [Idle gap])
  pure [Over SelAll (concat steps)]

-- | Short CPU requests with idle gaps.
serving :: Gen Plan
serving = do
  n <- sized (\size -> choose (2, max 6 (2 * size)))
  concat <$> replicateM n (request <$> choose (ms, 80 * ms) <*> choose (0, 300 * ms))
  where request d gap = [Over (SelDomains [Cpu]) [Idle d], Idle gap]

-- | Cells over all sources with idle stretches between: seconds while
-- someone reads the output, minutes while they think.
notebook :: Gen Plan
notebook = do
  n <- sized (\size -> choose (1, max 4 (size `div` 8)))
  concat <$> replicateM n (cell <$> runFor <*> thinkFor)
  where
    cell d gap = [Over SelAll [Idle d], Idle gap]
    runFor = frequency [(3, choose (100 * ms, 2 * sec)), (1, choose (2 * sec, 30 * sec))]
    thinkFor = frequency [(3, choose (0, 3 * sec)), (1, choose (3 * sec, 120 * sec))]

-- | Two threads on different sources whose cuts never coincide: one
-- thread's ops fall on even multiples of 2 ms, the other's 1 us after them.
jagged :: [SourceId] -> Gen Plan
jagged = \case
  (a : b : _) -> do
    ta <- thread a
    tb <- thread b
    pure [Both ta (Idle 1 : tb)]
  _ -> notebook
  where
    thread src = do
      n <- choose (1, 4)
      concat <$> replicateM n (do
        d <- step 1 100
        k <- choose (0, 2)
        gap <- step 0 50
        pure [Over (SelSources [src]) (concat (replicate k [Idle d, MarkHere]) ++ [Idle d]), Idle gap])
    step lo hi = (* (2 * ms)) <$> choose (lo, hi)

-- | Spans two or three deep, the middle one marked.
nested :: Gen Plan
nested = do
  let dur = choose (5 * ms, 300 * ms)
  (d1, d2, d3, d4, d5) <- (,,,,) <$> dur <*> dur <*> dur <*> dur <*> dur
  deep <- arbitrary
  let inner = [Over (SelDomains [Cpu]) [Idle d3] | deep]
  pure [Over SelAll [Idle d1, Over (SelDomains [Cpu, Ram]) ([Idle d2] ++ inner ++ [Idle d4, MarkHere]), Idle d5]]

-- | A child on sources its parent did not select: a GPU step under a CPU
-- phase, so the parent's own segments settle long before the child's.
disjoint :: Gen Plan
disjoint = do
  let dur = choose (ms, 200 * ms)
  (d1, d2, d3) <- (,,) <$> dur <*> dur <*> dur
  child <- elements [[Gpu], [Ram], [Gpu, Ram]]
  pure [Over (SelDomains [Cpu]) [Idle d1, Over (SelDomains child) [Idle d2], Idle d3]]

-- | A span whose selection names a domain nothing on this host serves.
unserved :: [Provider] -> Gen Plan
unserved ps
  | any ((== Gpu) . snd) (powerSources ps) = notebook
  | otherwise = (\d -> [Over (SelDomains [Gpu]) [Idle d]]) <$> choose (10 * ms, sec)

-- | Spans far shorter than the readout interval, at any phase against the
-- watchdog: most see one sample or none, a few straddle a watchdog read.
blink :: Gen Plan
blink = do
  n <- sized (\size -> choose (1, max 6 (size `div` 3)))
  concat <$> replicateM n (do
    lead <- choose (0, 2 * sec)
    d <- frequency [(3, choose (100, 5 * ms)), (1, choose (5 * ms, 40 * ms))]
    sel <- elements [SelAll, SelDomains [Cpu], SelDomains [Gpu]]
    pure [Idle lead, Over sel [Idle d]])

-- | Spans over arbitrary subsets of the host's power sources, some nested,
-- so cuts on many sources rarely line up.
subsets :: [Provider] -> Gen Plan
subsets ps = sized $ \n -> do
  let srcs = map fst (powerSources ps)
      pick = SelSources <$> (sublistOf srcs `suchThat` (not . null))
      one depth = do
        sel <- pick
        d <- choose (ms, 300 * ms)
        inner <- if depth > 0 then frequency [(2, pure []), (1, one (depth - 1 :: Int))] else pure []
        gap <- choose (0, 50 * ms)
        pure [Over sel (Idle d : inner), Idle gap]
  k <- choose (1, max 1 (min 12 (n `div` 6)))
  concat <$> replicateM k (one 2)

-- | Steps in an epoch: mostly a handful, often a real epoch of a hundred
-- or more once the size allows.
stepCount :: Gen Int
stepCount = sized $ \n -> frequency
  [(1, choose (2, 8)), (1, choose (8, max 8 (2 * n)))]

-- | How long one step of a run takes: tens of milliseconds for a small
-- model, up to seconds for a large one.
stepTime :: Gen Us
stepTime = frequency [(3, choose (20 * ms, 200 * ms)), (1, choose (200 * ms, 1500 * ms))]

-- | Within a tenth of @d@: steps of one run take about as long.
nearly :: Us -> Gen Us
nearly d = choose (d - d `div` 10, d + d `div` 10)

-- | Threads all starting together, joined when the last ends.
parallel :: [Plan] -> Plan
parallel = \case
  [] -> []
  [p] -> p
  (p : rest) -> [Both p (parallel rest)]

-- | Data-parallel training: epochs over everything, each step one span per
-- GPU running at once, an eval pass after each epoch. Some runs time the
-- data loader too: a CPU and RAM span inside each GPU's step.
dataParallel :: [Provider] -> Gen Plan
dataParallel ps = sized $ \n -> do
  let gpus = [src | (src, Gpu) <- powerSources ps]
      perDevice = if null gpus then [SelDomains [Cpu]] else map (\g -> SelSources [g]) gpus
  epochs <- choose (1, max 1 (min 4 (n `div` 20)))
  steps <- stepCount
  base <- stepTime
  loader <- arbitrary
  epoch <- replicateM epochs $ do
    stepPlans <- replicateM steps $ do
      d <- nearly base
      jitter <- vectorOf (length perDevice) (choose (0, 5 * ms))
      load <- choose (ms, d `div` 4)
      gap <- choose (0, 2 * ms)
      let body t = if loader then [Over (SelDomains [Cpu, Ram]) [Idle load], Idle (t - load)] else [Idle t]
      pure (parallel [[Over sel (body (d + j))] | (sel, j) <- zip perDevice jitter] ++ [Idle gap])
    evalFor <- choose (50 * ms, 500 * ms)
    pure [Over SelAll (concat stepPlans ++ [MarkHere, Over SelAll [Idle evalFor]])]
  pure (concat epoch)

-- | A hyperparameter sweep: short training runs one after another.
sweep :: [Provider] -> Gen Plan
sweep ps = sized $ \n -> do
  trials <- choose (2, max 2 (n `div` 15))
  concat <$> replicateM trials (resize (max 1 (n `div` 3)) (dataParallel ps) >>= \t -> pure [Over SelAll t])

-- | Serving: waves of requests at once, each on one GPU or the CPU.
concurrentServing :: [Provider] -> Gen Plan
concurrentServing ps = sized $ \n -> do
  let devices = [SelSources [src] | (src, Gpu) <- powerSources ps] ++ [SelDomains [Cpu]]
  waves <- choose (1, max 1 (n `div` 5))
  concat <$> replicateM waves (do
    k <- choose (1, max 2 (n `div` 6))
    requests <- replicateM k (do
      sel <- elements devices
      lead <- choose (0, 20 * ms)
      d <- choose (5 * ms, 150 * ms)
      pure [Idle lead, Over sel [Idle d]])
    gap <- choose (0, sec)
    pure (parallel requests ++ [Idle gap]))

-- | Every shape, named, with its weight.
shapes :: [Provider] -> [(String, Int, Gen Plan)]
shapes ps =
  [ ("training", 4, training), ("serving", 3, serving), ("notebook", 2, notebook)
  , ("jagged", 2, jagged (map fst (powerSources ps))), ("nested", 2, nested)
  , ("unserved", 1, unserved ps), ("disjoint", 2, disjoint), ("blink", 2, blink)
  , ("subsets", 2, subsets ps), ("data parallel", 6, dataParallel ps)
  , ("sweep", 2, sweep ps), ("concurrent serving", 3, concurrentServing ps) ]

shape :: [Provider] -> Gen Plan
shape ps = frequency [(w, g) | (_, w, g) <- shapes ps]

-- | A whole session: a few shapes in turn, some two at once, or nothing but
-- a wait before `finish`. The parts share the size, so a session of one
-- shape, like a long training run, gets it all.
genProgram :: [Provider] -> Gen Plan
genProgram ps = frequency
  [ (1, (\d -> [Idle d]) <$> choose (0, 5 * sec))
  , (19, do
      n <- sized (\size -> frequency [(2, pure 1), (3, choose (1, max 1 (min 10 (size `div` 10))))])
      parts <- replicateM n $ scale (\size -> max 1 (2 * size `div` (n + 1))) $ do
        concurrent <- frequency [(1, pure True), (3, pure False)]
        p <- if concurrent then (\a b -> [Both a b]) <$> shape ps <*> shape ps else shape ps
        gap <- choose (0, sec)
        pure (p ++ [Idle gap])
      lead <- choose (0, 200 * ms)
      pure (Idle lead : concat parts)) ]

assemble :: Failures -> Bool -> ([Provider] -> Gen Plan) -> Gen Scenario
assemble f constant genPlan = do
  ps <- genProviders f
  pairs <- genPairs ps
  plan <- genPlan ps
  let horizon = maybe 0 opAt (lastMaybe (scheduled plan))
  truth <- genTruth constant ps horizon
  pue <- elements [1.0, 1.2, 1.58]
  pure (Scenario pue sec timeoutUs ps pairs truth plan)
  where lastMaybe xs = if null xs then Nothing else Just (last xs)

-- | Any realistic scenario, failures included.
genScenario :: Gen Scenario
genScenario = assemble WithFailures False genProgram

-- | Every read instant and successful, every provider Queue, truth constant:
-- the ledger total is known exactly.
genCleanScenario :: Gen Scenario
genCleanScenario = assemble NoFailures True genProgram

-- | Only the jagged shape, for checking that its cuts never coincide.
genJaggedScenario :: Gen Scenario
genJaggedScenario = assemble WithFailures False (jagged . map fst . powerSources)

-- | Failures and all, but every curve flat: each source's energy is known.
genConstantScenario :: Gen Scenario
genConstantScenario = assemble WithFailures True genProgram
