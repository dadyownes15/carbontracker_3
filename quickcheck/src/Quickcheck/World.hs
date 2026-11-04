-- | The world: piecewise-linear truth per source, flat beyond its ends, and
-- its exact integrals. This is the oracle; nothing here models the engine.
module Quickcheck.World
  ( Curve, valueAt, integral, energyJ, isConstant
  ) where

import Quickcheck.Json (Us)

-- | Points sorted by time.
type Curve = [(Us, Double)]

valueAt :: Curve -> Us -> Double
valueAt [] _ = 0
valueAt ((t0, v0) : rest) t
  | t <= t0 = v0
  | otherwise = walk (t0, v0) rest
  where
    walk (_, v) [] = v
    walk (ta, va) ((tb, vb) : more)
      | t <= tb = if tb == ta then vb
                  else va + (vb - va) * fromIntegral (t - ta) / fromIntegral (tb - ta)
      | otherwise = walk (tb, vb) more

-- | Exact integral from a to b of the curve, in value x us.
integral :: Curve -> Us -> Us -> Double
integral c a b
  | b <= a = 0
  | otherwise = sum [trap x y | (x, y) <- zip ts (drop 1 ts)]
  where
    ts = [a] ++ [t | (t, _) <- c, t > a, t < b] ++ [b]
    trap x y = (valueAt c x + valueAt c y) / 2 * fromIntegral (y - x)

-- | Energy in J of a power curve in W over [a, b] us.
energyJ :: Curve -> Us -> Us -> Double
energyJ c a b = integral c a b * 1e-6

isConstant :: Curve -> Bool
isConstant c = case map snd c of
  [] -> True
  (v : vs) -> all (== v) vs
