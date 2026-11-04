-- | What a caller does, in two forms.
--
-- A 'Plan' is first-order data: spans are blocks, so a span always stops
-- after it starts and its children sit inside it. Generators build plans and
-- shrinking edits them; no edit can make one ill-formed.
--
-- A 'Session' is the free monad of caller operations. Each operation carries
-- its continuation, and 'start' hands a span handle to it, so programs never
-- name span ids: they are polymorphic in the handle type, and only an
-- interpreter decides what a handle is. 'schedule' is the pure interpreter
-- that turns a session into timed ops; a live one runs the same session
-- against a real tracker.
module Quickcheck.Program
  ( -- * Plans
    Plan, Step (..), shrinkPlan
    -- * Sessions
  , Free (..), Call (..), Session
  , start, stop, mark, wait, par, within
  , session
    -- * Interpreters
  , schedule, scheduled
  ) where

import Control.Monad.Trans.State.Strict (State, evalState, state)
import Quickcheck.Json hiding (Session (..))

-- Plans -------------------------------------------------------------------

type Plan = [Step]

data Step
  = Over Selection Plan  -- ^ a span over the plan, stopped when it ends
  | MarkHere             -- ^ marks the innermost span; nothing at the top
  | Idle Us
  | Both Plan Plan       -- ^ two threads from the same instant, joined after
  deriving (Eq, Show)

-- | Smaller plans, each still a plan: a step goes, a span gives its body to
-- its parent, a selection widens to all, threads run one after the other,
-- a wait halves.
shrinkPlan :: Plan -> [Plan]
shrinkPlan steps =
  [ before ++ replacement ++ after
  | (before, x, after) <- splits steps
  , replacement <- [] : smaller x ]
  where
    smaller = \case
      Over sel body ->
        body : [[Over SelAll body] | sel /= SelAll] ++ [[Over sel b] | b <- shrinkPlan body]
      Both a b ->
        (a ++ b) : [[Both a' b] | a' <- shrinkPlan a] ++ [[Both a b'] | b' <- shrinkPlan b]
      Idle d -> [[Idle (d `div` 2)] | d > 1]
      MarkHere -> []
    splits xs = [(take i xs, x, drop (i + 1) xs) | (i, x) <- zip [0 ..] xs]

-- Sessions ----------------------------------------------------------------

data Free f a = Pure a | Free (f (Free f a))

instance Functor f => Functor (Free f) where
  fmap g (Pure a) = Pure (g a)
  fmap g (Free fa) = Free (fmap (fmap g) fa)

instance Functor f => Applicative (Free f) where
  pure = Pure
  Pure g <*> x = fmap g x
  Free fg <*> x = Free (fmap (<*> x) fg)

instance Functor f => Monad (Free f) where
  Pure a >>= k = k a
  Free fa >>= k = Free (fmap (>>= k) fa)

-- | One caller operation on spans of handle type @span@, and what follows.
data Call span k
  = Start (Maybe span) Selection (span -> k)
  | Stop span k
  | Mark span k
  | Wait Us k
  | Par (Session span ()) (Session span ()) k

instance Functor (Call span) where
  fmap f = \case
    Start p sel k -> Start p sel (f . k)
    Stop s k -> Stop s (f k)
    Mark s k -> Mark s (f k)
    Wait d k -> Wait d (f k)
    Par a b k -> Par a b (f k)

type Session span = Free (Call span)

start :: Maybe span -> Selection -> Session span span
start p sel = Free $ Start p sel pure

stop :: span -> Session span ()
stop s = Free $ Stop s $ pure ()

mark :: span -> Session span ()
mark s = Free $ Mark s $ pure ()

wait :: Us -> Session span ()
wait d = Free $ Wait d $ pure ()

-- | Two threads from the same instant; the session goes on once both end.
par :: Session span () -> Session span () -> Session span ()
par a b = Free $ Par a b $ pure ()

-- | A span around @body@, under @parent@: started, given to the body, and
-- stopped when the body ends.
within :: Maybe span -> Selection -> (span -> Session span a) -> Session span a
within parent sel body = do
  s <- start parent sel
  x <- body s
  stop s
  pure x

-- | A plan as a session, its spans nested as written.
session :: Plan -> Session span ()
session = go Nothing
  where
    go parent = mapM_ (one parent)
    one parent = \case
      Over sel body -> within parent sel (\s -> go (Just s) body)
      MarkHere -> maybe (pure ()) mark parent
      Idle d -> wait d
      Both a b -> par (go parent a) (go parent b)

-- Interpreters ------------------------------------------------------------

-- | The ops a session makes, in time order, then `finish` once every thread
-- has ended. Handles are span ids, handed out in the order spans start;
-- threads that start together run left first.
-- AC/TST-31
schedule :: Session SpanId () -> [Op]
schedule s = [Op t b | (t, b) <- events] ++ [Op end OpFinish]
  where (_, events, end) = evalState (run 0 s) 1

-- | 'schedule' of a plan.
scheduled :: Plan -> [Op]
scheduled = schedule . session

run :: Us -> Session SpanId a -> State SpanId (a, [(Us, OpBody)], Us)
run t = \case
  Pure x -> pure (x, [], t)
  Free (Start p sel k) -> do
    s <- state (\n -> (n, n + 1))
    emit (OpStart s p sel) (k s)
  Free (Stop s k) -> emit (OpStop s) k
  Free (Mark s k) -> emit (OpMark s) k
  Free (Wait d k) -> run (t + d) k
  Free (Par a b k) -> do
    (_, ea, ta) <- run t a
    (_, eb, tb) <- run t b
    (x, ek, tk) <- run (max ta tb) k
    pure (x, merge ea eb ++ ek, tk)
  where
    emit b k = do
      (x, es, end) <- run t k
      pure (x, (t, b) : es, end)

-- | Two time-ordered lists as one, the left first on ties.
merge :: [(Us, a)] -> [(Us, a)] -> [(Us, a)]
merge [] ys = ys
merge xs [] = xs
merge (x : xs) (y : ys)
  | fst y < fst x = y : merge (x : xs) ys
  | otherwise = x : merge xs (y : ys)
