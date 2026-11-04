-- | Runs the self-tests, then the trace properties against the replay
-- binary. QUICKCHECK_CASES sets the case count (default 200).
module Main (main) where

import Control.Monad (forM, unless)
import System.Environment (lookupEnv)
import System.Exit (exitFailure)
import Test.QuickCheck
import Quickcheck.Properties
import Quickcheck.Pure
import Quickcheck.Runner

main :: IO ()
main = do
  n <- maybe 200 read <$> lookupEnv "QUICKCHECK_CASES"
  let args = stdArgs { maxSuccess = n }
      run name p = do
        putStrLn ("\n-- " ++ name)
        isSuccess <$> quickCheckWithResult args p
  selfOk <- forM selfTests (uncurry run)
  pureBin <- findPure
  pureOk <- case pureBin of
    Nothing -> do
      putStrLn "\nreplay-pure not built: skipped (cargo build -p carbontracker --features replay --bin replay-pure)"
      mapM_ (\t -> putStrLn ("  skipped: " ++ ptName t)) pureTests
      pure []
    Just b -> do
      putStrLn ("\nreplay-pure: " ++ b)
      forM pureTests $ \t -> run (ptName t) (runPureTest b t)
  bin <- findReplay
  traceOk <- case bin of
    Nothing -> do
      putStrLn "\nreplay-live not built yet: skipped"
      mapM_ (\t -> putStrLn ("  skipped: " ++ ttName t)) traceTests
      pure []
    Just b -> do
      putStrLn ("\nreplay: " ++ b)
      forM traceTests $ \t -> run (ttName t) (runTraceTest b t)
  unless (and (selfOk ++ pureOk ++ traceOk)) exitFailure
