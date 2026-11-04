-- | Runs the Rust replay binary on one case. REPLAY_BIN names it; otherwise
-- target/debug/replay-live (the stateful replay, not built yet) at the workspace root is used when it exists.
module Quickcheck.Runner (findReplay, runReplay, findPure, runPure) where

import Data.Aeson (eitherDecode, encode)
import qualified Data.ByteString.Lazy.Char8 as BL
import Control.Monad (filterM)
import System.Directory (doesFileExist, findExecutable, getModificationTime)
import System.Environment (lookupEnv)
import System.Exit (ExitCode (..))
import System.Process (readProcessWithExitCode)
import Quickcheck.Json

findReplay :: IO (Maybe FilePath)
findReplay = do
  env <- lookupEnv "REPLAY_BIN"
  case env of
    Just p | not (null p) -> findExecutable p >>= \case
      Just e -> pure (Just e)
      Nothing -> do
        ok <- doesFileExist p
        pure (if ok then Just p else Nothing)
    _ -> do
      let p = "../target/debug/replay-live"
      ok <- doesFileExist p
      pure (if ok then Just p else Nothing)

runReplay :: FilePath -> Case -> IO (Either String Trace)
runReplay bin c = do
  (code, out, err) <- readProcessWithExitCode bin [] (BL.unpack (encode c))
  pure $ case code of
    ExitSuccess -> either (\e -> Left ("unreadable trace: " ++ e ++ "\n" ++ take 2000 out)) Right
                     (eitherDecode (BL.pack out))
    ExitFailure n -> Left ("replay exited " ++ show n ++ ": " ++ take 2000 err)

-- | replay-pure: REPLAY_PURE_BIN names it; otherwise the newer of the
-- release and debug builds in target/.
findPure :: IO (Maybe FilePath)
findPure = lookupEnv "REPLAY_PURE_BIN" >>= \case
  Just p -> do
    ok <- doesFileExist p
    pure (if ok then Just p else Nothing)
  Nothing -> do
    built <- filterM doesFileExist ["../target/release/replay-pure", "../target/debug/replay-pure"]
    stamped <- mapM (\p -> (,) <$> getModificationTime p <*> pure p) built
    pure (if null stamped then Nothing else Just (snd (maximum stamped)))

runPure :: FilePath -> PureCase -> IO (Either String PureTrace)
runPure bin c = do
  (code, out, err) <- readProcessWithExitCode bin [] (BL.unpack (encode c))
  pure $ case code of
    ExitSuccess -> either (\e -> Left ("unreadable trace: " ++ e ++ "\n" ++ take 2000 out)) Right
                     (eitherDecode (BL.pack out))
    ExitFailure n -> Left ("replay-pure exited " ++ show n ++ ": " ++ take 2000 err)
