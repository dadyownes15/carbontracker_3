{-# LANGUAGE OverloadedStrings #-}
-- | The replay format: one case in, one trace out. Every JSON type lives here.
module Quickcheck.Json
  ( Us, SourceId, SpanId, ProviderId
  , Policy (..), ReadBehaviour (..), Domain (..), SampleKind (..), Source (..)
  , Provider (..), Selection (..), OpBody (..), Op (..), Case (..)
  , Asker (..), Answer (..), Request (..), CutRecord (..), Segment (..)
  , Settled (..), Usage (..), Session (..), Refusal (..), Trace (..)
  , sourceId, rootSpan
  , PureSource (..), Relies, PureBody (..), PureEvent (..), PureCase (..)
  , PureSegment (..), PureSession (..), PureRefusal (..), PureTrace (..), SpanReport (..)
  ) where

import Data.Aeson
import Data.Aeson.Types (Parser)
import qualified Data.Map.Strict as M
import Data.Text (Text)
import qualified Data.Text as T

type Us = Int
type SourceId = Text
type SpanId = Int
type ProviderId = Int

-- | Span 0 is the root in claimants and ledgers.
rootSpan :: SpanId
rootSpan = 0

sourceId :: ProviderId -> Int -> SourceId
sourceId p s = T.pack (show p ++ "." ++ show s)

-- Input -----------------------------------------------------------------

data Policy = Queue | Skip deriving (Eq, Show)

data ReadBehaviour
  = ReadOk Us
  | ReadHang
  | ReadPanic Us
  | ReadEmpty Us
  deriving (Eq, Show)

data Domain = Cpu | Gpu | Ram | Ane deriving (Eq, Ord, Show, Enum, Bounded)

data SampleKind = Instant | Held | Total deriving (Eq, Show)

data Source
  = PowerSource { srcSlot :: Int, srcDomain :: Domain, srcSample :: SampleKind }
  | IntensitySource { srcSlot :: Int }
  deriving (Eq, Show)

data Provider = Provider
  { provId       :: ProviderId
  , provCooldown :: Maybe Us -- ^ Nothing is Never
  , provPolicy   :: Policy
  , provReads    :: [ReadBehaviour]
  , provSources  :: [Source]
  } deriving (Eq, Show)

data Selection = SelAll | SelDomains [Domain] | SelSources [SourceId]
  deriving (Eq, Show)

data OpBody
  = OpStart SpanId (Maybe SpanId) Selection
  | OpMark SpanId
  | OpStop SpanId
  | OpFinish
  deriving (Eq, Show)

data Op = Op { opAt :: Us, opBody :: OpBody } deriving (Eq, Show)

data Case = Case
  { casePue             :: Double
  , caseReadoutInterval :: Us
  , caseTimeout         :: Us
  , caseProviders       :: [Provider]
  , casePairs           :: M.Map SourceId SourceId
  , caseTruth           :: M.Map SourceId [(Us, Double)]
  , caseProgram         :: [Op]
  } deriving (Eq, Show)

instance ToJSON Policy where
  toJSON Queue = "queue"
  toJSON Skip = "skip"

instance ToJSON ReadBehaviour where
  toJSON (ReadOk l) = object ["latency_us" .= l]
  toJSON ReadHang = object ["hang" .= True]
  toJSON (ReadPanic l) = object ["panic_after_us" .= l]
  toJSON (ReadEmpty l) = object ["latency_us" .= l, "empty" .= True]

instance ToJSON Domain where
  toJSON Cpu = "cpu"
  toJSON Gpu = "gpu"
  toJSON Ram = "ram"
  toJSON Ane = "ane"

instance ToJSON SampleKind where
  toJSON Instant = "instant"
  toJSON Held = "held"
  toJSON Total = "total"

instance ToJSON Source where
  toJSON (PowerSource s d k) =
    object ["slot" .= s, "kind" .= ("power" :: Text), "domain" .= d, "sample" .= k]
  toJSON (IntensitySource s) = object ["slot" .= s, "kind" .= ("intensity" :: Text)]

instance ToJSON Provider where
  toJSON p = object
    [ "id" .= provId p, "cooldown_us" .= provCooldown p, "policy" .= provPolicy p
    , "reads" .= provReads p, "sources" .= provSources p ]

instance ToJSON Selection where
  toJSON SelAll = "all"
  toJSON (SelDomains ds) = object ["domains" .= ds]
  toJSON (SelSources ss) = object ["sources" .= ss]

instance ToJSON Op where
  toJSON (Op t b) = object (("at_us" .= t) : body b)
    where
      body (OpStart s p sel) =
        ["op" .= ("start" :: Text), "span" .= s, "parent" .= p, "selection" .= sel]
      body (OpMark s) = ["op" .= ("mark" :: Text), "span" .= s]
      body (OpStop s) = ["op" .= ("stop" :: Text), "span" .= s]
      body OpFinish = ["op" .= ("finish" :: Text)]

instance ToJSON Case where
  toJSON c = object
    [ "pue" .= casePue c
    , "readout_interval_us" .= caseReadoutInterval c
    , "timeout_us" .= caseTimeout c
    , "providers" .= caseProviders c
    , "pairs" .= casePairs c
    , "truth" .= caseTruth c
    , "program" .= caseProgram c ]

-- Output ----------------------------------------------------------------

data Asker = AskCut | AskWatchdog deriving (Eq, Show)

data Answer = AnsRead | AnsUnchanged | AnsSkipped | AnsFailed | AnsDone
  deriving (Eq, Show)

data Request = Request
  { reqId       :: Int
  , reqProvider :: ProviderId
  , reqAsker    :: Asker
  , reqSent     :: Us
  , reqStarted  :: Maybe Us
  , reqAnswered :: Maybe Us
  , reqAnswer   :: Maybe Answer
  , reqQueued   :: Bool        -- ^ waited for the provider to be ready
  , reqFailure  :: Maybe Text  -- ^ why a failed answer failed
  } deriving (Eq, Show)

data CutRecord = CutRecord
  { cutAt      :: Us
  , cutOpIndex :: Int
  , cutRelied  :: M.Map SourceId (Maybe Int)
  } deriving (Eq, Show)

data Segment = Segment
  { segSource    :: SourceId
  , segFrom      :: Us
  , segTo        :: Us
  , segClaimants :: [SpanId]
  , segAccounted :: Maybe Us
  , segEnergy    :: Double
  , segEmissions :: Maybe Double
  } deriving (Eq, Show)

data Settled = Settled { settledSpan :: SpanId, settledAt :: Us } deriving (Eq, Show)

data Usage = Usage { usageEnergy :: Double, usageEmissions :: Maybe Double }
  deriving (Eq, Show)

data Session = Session
  { sessionEnergy    :: Double
  , sessionEmissions :: Maybe Double
  , sessionEnded     :: Maybe Us
  } deriving (Eq, Show)

data Refusal = Refusal { refusedOpIndex :: Int, refusedError :: Text } deriving (Eq, Show)

data Trace = Trace
  { traceRequests :: [Request]
  , traceCuts     :: [CutRecord]
  , traceSegments :: [Segment]
  , traceSettled  :: [Settled]
  , traceLedgers  :: M.Map Text (M.Map SourceId Usage)
  , traceSession  :: Session
  , traceRefused  :: [Refusal]
  } deriving (Eq, Show)

instance FromJSON Asker where
  parseJSON = withText "asker" $ \case
    "cut" -> pure AskCut
    "watchdog" -> pure AskWatchdog
    other -> fail ("unknown asker " ++ show other)

instance FromJSON Answer where
  parseJSON = withText "answer" $ \case
    "read" -> pure AnsRead
    "unchanged" -> pure AnsUnchanged
    "skipped" -> pure AnsSkipped
    "failed" -> pure AnsFailed
    "done" -> pure AnsDone
    other -> fail ("unknown answer " ++ show other)

instance FromJSON Request where
  parseJSON = withObject "request" $ \o -> Request
    <$> o .: "id" <*> o .: "provider" <*> o .: "asker" <*> o .: "sent_us"
    <*> o .:? "started_us" <*> o .:? "answered_us" <*> o .:? "answer"
    <*> o .:? "queued" .!= False <*> o .:? "failure"

instance FromJSON CutRecord where
  parseJSON = withObject "cut" $ \o -> CutRecord
    <$> o .: "at_us" <*> o .: "op_index" <*> o .: "relied"

instance FromJSON Segment where
  parseJSON = withObject "segment" $ \o -> Segment
    <$> o .: "source" <*> o .: "from_us" <*> o .: "to_us" <*> o .: "claimants"
    <*> o .:? "accounted_us" <*> o .: "energy_j" <*> o .:? "emissions_g"

instance FromJSON Settled where
  parseJSON = withObject "settled" $ \o -> Settled <$> o .: "span" <*> o .: "at_us"

instance FromJSON Usage where
  parseJSON = withObject "usage" $ \o -> Usage <$> o .: "energy_j" <*> o .:? "emissions_g"

instance FromJSON Session where
  parseJSON = withObject "session" $ \o -> Session
    <$> o .: "total_energy_j" <*> o .:? "total_emissions_g" <*> o .:? "ended_us"

instance FromJSON Refusal where
  parseJSON = withObject "refusal" $ \o -> Refusal <$> o .: "op_index" <*> o .: "error"

instance FromJSON Trace where
  parseJSON = withObject "trace" $ \o -> Trace
    <$> o .: "requests" <*> o .: "cuts" <*> o .: "segments" <*> o .: "settled"
    <*> (o .: "ledgers" :: Parser (M.Map Text (M.Map SourceId Usage)))
    <*> o .: "session" <*> o .: "refused"

-- Pure replay -----------------------------------------------------------
-- One case for replay-pure: the catalog, and the events the session sees in
-- order, with what each cut relies on and each readout's samples.

data PureSource
  = PurePower SourceId Domain SampleKind
  | PureIntensity SourceId
  deriving (Eq, Show)

-- | Per provider, the request a cut relies on; a provider left out needs none.
type Relies = M.Map ProviderId Int

data PureBody
  = PStart SpanId (Maybe SpanId) Selection Relies
  | PMark SpanId Relies
  | PStop SpanId Relies
  | PFinish Relies
  | PLand Int (M.Map SourceId [(Us, Double)])
  deriving (Eq, Show)

data PureEvent = PureEvent { peAt :: Us, peBody :: PureBody } deriving (Eq, Show)

data PureCase = PureCase
  { pcPue     :: Double
  , pcSources :: [PureSource]
  , pcPairs   :: M.Map SourceId SourceId
  , pcEvents  :: [PureEvent]
  } deriving (Eq, Show)

instance ToJSON PureSource where
  toJSON (PurePower i d k) = object ["kind" .= ("power" :: Text), "id" .= i, "domain" .= d, "sample" .= k]
  toJSON (PureIntensity i) = object ["kind" .= ("intensity" :: Text), "id" .= i]

instance ToJSON PureEvent where
  toJSON (PureEvent t b) = object (("at_us" .= t) : body b)
    where
      body (PStart s p sel r) =
        ["op" .= ("start" :: Text), "span" .= s, "parent" .= p, "selection" .= sel, "relies" .= r]
      body (PMark s r) = ["op" .= ("mark" :: Text), "span" .= s, "relies" .= r]
      body (PStop s r) = ["op" .= ("stop" :: Text), "span" .= s, "relies" .= r]
      body (PFinish r) = ["op" .= ("finish" :: Text), "relies" .= r]
      body (PLand q ss) = ["op" .= ("land" :: Text), "request" .= q, "samples" .= ss]

instance ToJSON PureCase where
  toJSON c = object
    [ "pue" .= pcPue c, "sources" .= pcSources c, "pairs" .= pcPairs c, "events" .= pcEvents c ]

data PureSegment = PureSegment
  { psSource    :: SourceId
  , psFrom      :: Us
  , psTo        :: Us
  , psClaimants :: [SpanId]
  , psAccounted :: Us
  , psEnergy    :: Double
  , psEmissions :: Maybe Double
  } deriving (Eq, Show)

data PureSession = PureSession
  { pssEnergy    :: Double
  , pssEmissions :: Maybe Double
  , pssDrained   :: Bool
  } deriving (Eq, Show)

data PureRefusal = PureRefusal { prEvent :: Int, prError :: Text } deriving (Eq, Show)

-- | A settled span's report: its samples per selected power source, and the
-- kinds of warning it gave.
data SpanReport = SpanReport
  { srSpan     :: SpanId
  , srSamples  :: M.Map SourceId Int
  , srWarnings :: [Text]
  } deriving (Eq, Show)

instance FromJSON SpanReport where
  parseJSON = withObject "span" $ \o -> SpanReport <$> o .: "span" <*> o .: "samples" <*> o .: "warnings"

data PureTrace = PureTrace
  { ptSegments :: [PureSegment]
  , ptSettled  :: [Settled]
  , ptSpans    :: [SpanReport]
  , ptLedgers  :: M.Map Text (M.Map SourceId Usage)
  , ptSession  :: PureSession
  , ptRefused  :: [PureRefusal]
  } deriving (Eq, Show)

instance FromJSON PureSegment where
  parseJSON = withObject "segment" $ \o -> PureSegment
    <$> o .: "source" <*> o .: "from_us" <*> o .: "to_us" <*> o .: "claimants"
    <*> o .: "accounted_us" <*> o .: "energy_j" <*> o .:? "emissions_g"

instance FromJSON PureSession where
  parseJSON = withObject "session" $ \o -> PureSession
    <$> o .: "total_energy_j" <*> o .:? "total_emissions_g" <*> o .: "drained"

instance FromJSON PureRefusal where
  parseJSON = withObject "refusal" $ \o -> PureRefusal <$> o .: "event" <*> o .: "error"

instance FromJSON PureTrace where
  parseJSON = withObject "trace" $ \o -> PureTrace
    <$> o .: "segments" <*> o .: "settled" <*> o .: "spans" <*> o .: "ledgers" <*> o .: "session" <*> o .: "refused"
