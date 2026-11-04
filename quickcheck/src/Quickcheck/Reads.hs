-- | The providers' side of a case, as the world sees it: when each provider
-- reads, what it reads, and which request each cut relies on. It turns a
-- 'Case' (program, providers, truth) into the 'PureCase' replay-pure runs.
--
-- This is input, not an oracle. It follows the rules of 08 closely enough to
-- be realistic (cooldowns, Queue and Skip, one queued request, timeouts,
-- panics, the watchdog, Never providers), and no property relies on it being
-- exact: the properties read only the truth and what the session did.
module Quickcheck.Reads
  ( Built (..), Reliance (..), toPure
  ) where

import Data.List (sortOn)
import qualified Data.Map.Strict as M
import qualified Data.Set as S
import qualified Data.Text as T
import Quickcheck.Json
import Quickcheck.Scenario (resolve)
import Quickcheck.World

-- | How a cut came to rely on its request, for coverage labels.
data Reliance
  = ReadNow      -- ^ a read started at the cut
  | ReadOut      -- ^ a read already out (started at the cut, or a Never provider's)
  | Queued       -- ^ a request queued for the provider to be ready
  | AnsweredNow  -- ^ skipped, or failed at once, with no read
  deriving (Eq, Ord, Show)

data Built = Built
  { builtCase     :: PureCase
  , builtReliance :: [Reliance]
  , builtTimeouts :: Int  -- ^ requests that failed by timeout
  , builtPanics   :: Int
  , builtWatchdog :: Int  -- ^ watchdog reads started
  , builtFinish   :: Maybe Us
  }

-- | One read: when it starts, when the request is answered, when the
-- provider is home again (Nothing: never), and what it answers.
data Reading = Reading
  { rStart   :: Us
  , rRequest :: Int
  , rAnswer  :: Us
  , rHome    :: Maybe Us
  , rSamples :: Bool  -- ^ answered every source with samples
  , rPanics  :: Bool
  , rTimedOut :: Bool
  }

data PState = PState
  { stReads     :: [Reading]   -- ^ newest first
  , stBehaviour :: Int
  , stLastSent  :: Us
  , stWatchdog  :: Us
  }

data Acc = Acc
  { accNext     :: Int
  , accStates   :: M.Map ProviderId PState
  , accEvents   :: [(Us, Int, PureBody)]  -- ^ time, sequence, body
  , accSeq      :: Int
  , accReliance :: [Reliance]
  , accWatchdog :: Int
  }

-- AC/TST-34
toPure :: Case -> Built
toPure c = Built
  { builtCase = PureCase
      { pcPue = casePue c
      , pcSources = sources
      , pcPairs = casePairs c
      , pcEvents = [PureEvent t b | (t, _, b) <- sortOn (\(t, q, _) -> (t, q)) (accEvents final)] }
  , builtReliance = reverse (accReliance final)
  , builtTimeouts = length [() | r <- allReads, rTimedOut r]
  , builtPanics = length [() | r <- allReads, rPanics r]
  , builtWatchdog = accWatchdog final
  , builtFinish = finishAt }
  where
    ps = caseProviders c
    byId = M.fromList [(provId p, p) | p <- ps]
    sources =
      [ case s of
          PowerSource slot d k -> PurePower (sourceId (provId p) slot) d k
          IntensitySource slot -> PureIntensity (sourceId (provId p) slot)
      | p <- ps, s <- provSources p ]
    ops = zip [0 :: Int ..] (caseProgram c)
    finishAt = case [t | Op t OpFinish <- caseProgram c] of
      (t : _) -> Just t
      [] -> Nothing
    selections = M.fromList [(s, sel) | Op _ (OpStart s _ sel) <- caseProgram c]

    -- The providers a cut asks: those of the sources it cuts, and of their
    -- intensity sources.
    asked = \case
      OpFinish -> M.keys byId
      OpStart s _ _ -> spanProviders s
      OpMark s -> spanProviders s
      OpStop s -> spanProviders s
    spanProviders s =
      let power = S.toList (resolve ps (M.findWithDefault SelAll s selections))
          intensity = [i | p <- power, Just i <- [M.lookup p (casePairs c)]]
      in S.toList (S.fromList [providerOfSource x | x <- power ++ intensity])
    providerOfSource x = read (takeWhile (/= '.') (T.unpack x)) :: ProviderId

    start0 = Acc 1 (M.fromList [(provId p, PState [] 0 0 0) | p <- ps]) [] 0 [] 0
    opened = foldl' (\a p -> snd (startRead p 0 True a)) start0 (map provId ps)
    final = foldl' (\a (i, Op t body) -> cutAt i t body (watchdogsBefore t a)) opened ops
    allReads = concatMap stReads (M.elems (accStates final))

    -- The ops in order, each cut asking the providers of what it cuts.
    cutAt i t body a =
      let ask (m, acc) p = let (r, acc') = cut p t acc in (maybe m (\q -> M.insert p q m) r, acc')
          (relies, a') = foldl' ask (M.empty, a) (asked body)
      in emit t (pureOp i body relies) a'

    pureOp _ body r = case body of
      OpStart s p sel -> PStart s p sel r
      OpMark s -> PMark s r
      OpStop s -> PStop s r
      OpFinish -> PFinish r

    emit t b a = a { accEvents = (t, accSeq a, b) : accEvents a, accSeq = accSeq a + 1 }

    cooldownOf p = maybe 0 id (provCooldown (byId M.! p))
    never p = provCooldown (byId M.! p) == Nothing
    policy p = provPolicy (byId M.! p)
    timeout = caseTimeout c
    period p = max (caseReadoutInterval c) (cooldownOf p)

    st p a = accStates a M.! p
    retired p t a = or [rPanics r && maybe False (<= t) (rHome r) | r <- stReads (st p a)]
    done p t a = never p && or [rSamples r && rAnswer r <= t | r <- stReads (st p a)]
    -- The read out at t: started, not yet home.
    out p t a = [r | r <- stReads (st p a), rStart r <= t, maybe True (> t) (rHome r)]
    queued p t a = [r | r <- stReads (st p a), rStart r > t]
    lastStart p a = case stReads (st p a) of
      (r : _) -> Just (rStart r)
      [] -> Nothing
    cooledAt p a = maybe 0 (+ cooldownOf p) (lastStart p a)

    note rel a = a { accReliance = rel : accReliance a }

    -- What a cut at t does about provider p: the request it relies on.
    cut p t a
      | done p t a = (Nothing, a)
      | retired p t a = answerNow p t a
      | (q : _) <- queued p t a = (Just (rRequest q), note Queued a)
      | (r : _) <- out p t a =
          if t >= rStart r + timeout then answerNow p t a
          else if rStart r == t || never p then (Just (rRequest r), note ReadOut a)
          else unready p t (maybe t id (rHome r)) a
      | t >= cooledAt p a = let (q, a') = startRead p t True a in (Just q, note ReadNow a')
      | otherwise = unready p t t a
    unready p t home a = case policy p of
      Queue ->
        let s = max home (cooledAt p a)
            (q, a') = startRead p s True a
        in (Just q, note Queued a')
      Skip -> answerNow p t a

    -- A request answered at once with nothing: skipped, or failed.
    answerNow p t a =
      let q = accNext a
          a' = (emit t (PLand q M.empty) a) { accNext = q + 1 }
      in (Just q, note AnsweredNow (sent p t a'))

    sent p t a = a { accStates = M.adjust (\s -> s { stLastSent = max (stLastSent s) t, stWatchdog = t + period p }) p (accStates a) }

    -- A read of p starting at s for a fresh request; its readout is emitted.
    -- A cut's request (the opening read included) under Queue is not
    -- answered by a failed read: that failure lands under a fresh id, and
    -- the same request is read again once the provider is home and cooled,
    -- giving up `retryFor` after it was first sent (08, when a read fails).
    startRead p s isCut a = readFor p s isCut Nothing a

    readFor p s isCut reuse a =
      let prov = byId M.! p
          q = maybe (accNext a) fst reuse
          sentAt = maybe s snd reuse
          a0 = case reuse of
            Nothing -> a { accNext = accNext a + 1 }
            Just _ -> a
          s0 = st p a0
          bs = provReads prov
          b = if null bs then ReadOk 0 else bs !! min (stBehaviour s0) (length bs - 1)
          samples = M.fromList
            [ (sid, [(s, value k sid)]) | src <- provSources prov
            , let sid = sourceId p (srcSlot src)
            , let k = case src of PowerSource _ _ kind -> Just kind; IntensitySource _ -> Nothing ]
          value k sid = let curve = M.findWithDefault [] sid (caseTruth c) in case k of
            Just Total -> energyJ curve 0 s
            _ -> valueAt curve s
          (r, land) = case b of
            ReadOk l | l <= timeout -> (Reading s q (s + l) (Just (s + l)) True False False, PLand q samples)
            ReadOk l -> (Reading s q (s + timeout) (Just (s + l)) False False True, PLand q M.empty)
            ReadEmpty l | l <= timeout -> (Reading s q (s + l) (Just (s + l)) False False False, PLand q M.empty)
            ReadEmpty l -> (Reading s q (s + timeout) (Just (s + l)) False False True, PLand q M.empty)
            ReadHang -> (Reading s q (s + timeout) Nothing False False True, PLand q M.empty)
            ReadPanic l -> (Reading s q (s + min l timeout) (Just (s + l)) False True (l > timeout), PLand q M.empty)
          failed = case b of
            ReadOk l -> l > timeout
            ReadEmpty _ -> True
            ReadHang -> True
            ReadPanic _ -> False
          a1 = a0 { accStates = M.insert p s0 { stReads = r : stReads s0, stBehaviour = stBehaviour s0 + 1 } (accStates a0) }
      in if not (isCut && policy p == Queue && failed)
           then (q, sent p s (emit (rAnswer r) land a1))
           else
             let fresh = accNext a1
                 a2 = (emit (rAnswer r) (PLand fresh M.empty) a1) { accNext = fresh + 1 }
                 next = fmap (\h -> max h (s + cooldownOf p)) (rHome r)
                 giveUp = max (rAnswer r) (sentAt + retryFor)
             in case next of
                  Just n | n < sentAt + retryFor ->
                    let (_, a3) = readFor p n True (Just (q, sentAt)) (sent p s a2) in (q, a3)
                  _ -> (q, sent p s (emit giveUp (PLand q M.empty) a2))

    retryFor = 30 * 1000000

    -- Every watchdog read due before t, at the instant it is due. A provider
    -- due while it is unready is asked once it is home and cooled; one that is
    -- done, retired or hung is never asked again.
    watchdogsBefore t a = case nextDue a of
      Just (d, p) | d < t -> watchdogsBefore t (fire p d a)
      _ -> a
    nextDue a = case [(stWatchdog s, p) | (p, s) <- M.toList (accStates a), stWatchdog s < never'] of
      [] -> Nothing
      due -> Just (minimum due)
    never' = maxBound `div` 2
    fire p d a
      | done p d a || retired p d a = defer never' a
      | (r : _) <- out p d a ++ queued p d a = defer (maybe never' (max (d + 1)) (rHome r)) a
      | d < cooledAt p a = defer (cooledAt p a) a
      | otherwise = let (_, a') = startRead p d False a in a' { accWatchdog = accWatchdog a' + 1 }
      where defer at acc = acc { accStates = M.adjust (\s -> s { stWatchdog = at }) p (accStates acc) }
